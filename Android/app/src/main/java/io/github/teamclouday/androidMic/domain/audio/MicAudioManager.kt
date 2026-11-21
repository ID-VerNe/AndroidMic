package io.github.teamclouday.androidMic.domain.audio

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.AudioEffect
import android.media.audiofx.AutomaticGainControl
import android.media.audiofx.NoiseSuppressor
import android.os.Build
import android.os.Process
import android.util.Log
import androidx.core.content.ContextCompat
import io.github.teamclouday.androidMic.domain.service.AudioPacket
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.channelFlow
import kotlinx.coroutines.launch
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.Executors


private const val TAG: String = "MicAM"

// manage microphone recording
class MicAudioManager(
    ctx: Context,
    val scope: CoroutineScope,
    val sampleRate: Int,
    val audioFormat: Int,
    val channelCount: Int,
    val audioSource: Int,
    // use the unprocessed source even when the device doesn't advertise support for it
    val forceUnprocessed: Boolean = false,
    private val gate: CaptureGate = CaptureGate(),
) {

    companion object {
        const val RECORD_DELAY_MS = 100L

        // audio is sent in packets this long, the PC can't play it before a packet is complete
        const val CHUNK_MS = 5

        // the record buffer is kept drained so its size doesn't add latency,
        // a big one just survives longer scheduling or network hiccups without losing audio
        const val RECORD_BUFFER_MS = 500

        // after capture goes idle, keep sending generated silence this long so the PC
        // playout doesn't have to rebuffer; then stop sending entirely
        const val SILENCE_KEEPALIVE_MS = 1500L
    }

    private var recorder: AudioRecord? = null
    private var effects: List<AudioEffect> = emptyList()
    private val chunkSize: Int
    private val buffer: ByteArray
    private val bufferFloat: FloatArray
    private val bufferFloatConvert: ByteBuffer
    private val silenceBuffer: ByteArray
    private var streamJob: Job? = null

    // recorder configuration kept for error recovery rebuilds
    private var channelConfig: Int = 0
    private var bufferSize: Int = 0
    private var minBufferSize: Int = 0
    private var currentSource: Int = audioSource
    private var unprocessedRequested: Boolean = false

    // minimum interval between recorder rebuild attempts, keeps a failing device from
    // retrying every 5 ms chunk and flooding the log
    private val rebuildRetryIntervalMs = 1000L
    private var lastRebuildAttemptMs = 0L

    // reading the mic on a normal priority thread adds scheduling jitter
    private val captureDispatcher = Executors.newSingleThreadExecutor { r ->
        Thread({
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
            r.run()
        }, "AndroidMic capture")
    }.asCoroutineDispatcher()

    private var isMuted = false

    // wall-clock deadline until which generated silence keeps flowing after capture goes idle
    private var keepaliveUntilMs = 0L

    // per-variant recorder hold strategy: immediate in release, experimental
    // grace/always policies in debug (resolved by the source-set-specific factory)
    private val holdStrategy: RecorderHoldStrategy =
        provideHoldStrategy({ System.currentTimeMillis() }, ctx)

    // true when the unprocessed source was requested but the device can't provide it
    var isUnprocessedFallback = false
        private set

    init {
        // check microphone
        require(ctx.packageManager.hasSystemFeature(PackageManager.FEATURE_MICROPHONE)) {
            "Microphone is not detected on this device"
        }
        require(
            ContextCompat.checkSelfPermission(
                ctx,
                Manifest.permission.RECORD_AUDIO
            ) == PackageManager.PERMISSION_GRANTED
        ) {
            "Microphone recording is not permitted"
        }

        // get minimum buffer size
        channelConfig =
            if (channelCount == 2) AudioFormat.CHANNEL_IN_STEREO else AudioFormat.CHANNEL_IN_MONO
        minBufferSize = AudioRecord.getMinBufferSize(
            sampleRate,
            channelConfig,
            audioFormat,
        )

        require(minBufferSize != AudioRecord.ERROR && minBufferSize != AudioRecord.ERROR_BAD_VALUE) {
            "Microphone buffer size ($minBufferSize) is invalid\nAudio format is likely not supported"
        }

        val frameSize = channelCount * bytesPerSample(audioFormat)
        chunkSize = maxOf(1, sampleRate * CHUNK_MS / 1000) * frameSize
        val bufferFrames = maxOf(
            minBufferSize * 2 / frameSize,
            sampleRate * RECORD_BUFFER_MS / 1000
        )
        bufferSize = bufferFrames * frameSize

        unprocessedRequested = isUnprocessedSource(audioSource)

        // the unprocessed source behaves like the default (processed) one when the device
        // doesn't support it, voice recognition is a better match in that case
        var source = audioSource
        if (unprocessedRequested && !forceUnprocessed && !isUnprocessedSupported(ctx)) {
            Log.w(TAG, "unprocessed audio source not supported, using voice recognition")
            source = MediaRecorder.AudioSource.VOICE_RECOGNITION
            isUnprocessedFallback = true
        }
        currentSource = source

        var rec = createRecorder(source)

        if (rec.state != AudioRecord.STATE_INITIALIZED && !isUnprocessedFallback && unprocessedRequested) {
            Log.w(TAG, "unprocessed audio source failed to initialize, using voice recognition")
            rec.release()
            currentSource = MediaRecorder.AudioSource.VOICE_RECOGNITION
            isUnprocessedFallback = true
            rec = createRecorder(currentSource)
        }
        recorder = rec

        // check if recorder is initialized
        require(recorder?.state == AudioRecord.STATE_INITIALIZED) {
            "Microphone recording failed to initialize"
        }

        effects = if (unprocessedRequested) disablePreProcessing(recorder!!.audioSessionId) else emptyList()

        buffer = ByteArray(chunkSize)
        bufferFloat = FloatArray(chunkSize / 4) // float is 4 bytes
        bufferFloatConvert = ByteBuffer.allocate(chunkSize).order(ByteOrder.nativeOrder())
        silenceBuffer = ByteArray(chunkSize).apply { fill(silence(audioFormat)) }

        Log.d(TAG, "record buffer: $bufferSize bytes, chunk: $chunkSize bytes")
    }

    // init recorder, with the minimum buffer if the device refuses a big one
    private fun createRecorder(source: Int): AudioRecord {
        val rec = AudioRecord(source, sampleRate, channelConfig, audioFormat, bufferSize)
        if (rec.state == AudioRecord.STATE_INITIALIZED || bufferSize <= minBufferSize) {
            return rec
        }
        Log.w(TAG, "record buffer of $bufferSize bytes refused, using $minBufferSize")
        rec.release()
        return AudioRecord(source, sampleRate, channelConfig, audioFormat, minBufferSize)
    }

    // audio stream publisher
    fun audioStream(): Flow<AudioPacket> = channelFlow {
        // launch in scope so infinite loop will be canceled when scope exits
        streamJob = scope.launch(captureDispatcher) {
            var wasCapturing = true
            while (true) {

                val capturing = gate.update(isMuted)

                if (capturing) {
                    holdStrategy.onActive()
                    keepaliveUntilMs = 0L
                    setCapturing(true)

                    val rec = recorder
                    if (rec == null || rec.state != AudioRecord.STATE_INITIALIZED ||
                        rec.recordingState != AudioRecord.RECORDSTATE_RECORDING
                    ) {
                        delay(RECORD_DELAY_MS)
                        continue
                    }

                    val packetBuffer = readPacket(rec)
                    if (packetBuffer == null) {
                        delay(RECORD_DELAY_MS)
                        continue
                    }

                    // legacy PC (no on-demand handshake): keep reading the mic and fill silence
                    // while muted, exactly like the pre-on-demand behaviour
                    if (!gate.linked && isMuted) {
                        packetBuffer.fill(silence(audioFormat))
                    }

                    send(
                        AudioPacket(
                            buffer = packetBuffer,
                            sampleRate = sampleRate,
                            audioFormat = audioFormat,
                            channelCount = channelCount
                        )
                    )
                } else {
                    val now = System.currentTimeMillis()
                    holdStrategy.onIdle(now)

                    if (holdStrategy.holdsOpenWhenIdle()) {
                        // hold the recorder open for the whole connection: keep reading the mic
                        // but drop the packet locally, the gate already withheld audio from the PC
                        setCapturing(true)

                        val rec = recorder
                        if (rec == null || rec.state != AudioRecord.STATE_INITIALIZED ||
                            rec.recordingState != AudioRecord.RECORDSTATE_RECORDING
                        ) {
                            delay(RECORD_DELAY_MS)
                            continue
                        }

                        if (readPacket(rec) == null) {
                            delay(RECORD_DELAY_MS)
                            continue
                        }
                        delay(CHUNK_MS.toLong())
                    } else {
                        // idle: don't block on read, keep the TCP stream alive
                        setCapturing(false)

                        if (wasCapturing) {
                            keepaliveUntilMs = now + SILENCE_KEEPALIVE_MS
                        }

                        if (now < keepaliveUntilMs) {
                            send(silencePacket())
                            delay(CHUNK_MS.toLong())
                        } else {
                            // strategies that release after their idle window drop the
                            // recorder here; immediate keeps the baseline retention after the tail
                            val decision = holdStrategy.evaluate(now, keepaliveUntilMs)
                            if (decision == HoldDecision.Release &&
                                holdStrategy.releasesRecorderWhenIdle()
                            ) {
                                releaseIdleRecorder()
                            }
                            delay(RECORD_DELAY_MS)
                        }
                    }
                }

                wasCapturing = capturing
            }
        }

        awaitClose {
            streamJob?.cancel()
        }
    }

    fun mute() {
        isMuted = true
    }

    fun unmute() {
        isMuted = false
    }

    // start recording
    fun start() {
        setCapturing(true)
        Log.d(TAG, "start")
    }

    // stop recording
    fun stop() {
        setCapturing(false)
        Log.d(TAG, "stop")
    }

    /**
     * Drive the recorder recording state to match [recording]. On an [IllegalStateException]
     * or an uninitialized recorder, release and rebuild with the init parameters; if the
     * rebuild still fails, the recorder field stays null and retries are throttled, failing
     * open instead of going silently mute.
     */
    fun setCapturing(recording: Boolean) {
        val rec = recorder ?: run {
            rebuildRecorder()
            return
        }
        try {
            if (recording) {
                if (rec.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
                    rec.startRecording()
                }
            } else {
                if (rec.recordingState == AudioRecord.RECORDSTATE_RECORDING) {
                    rec.stop()
                }
            }
        } catch (e: IllegalStateException) {
            Log.w(TAG, "recorder state error (capturing=$recording): ${e.message}; rebuilding")
            rebuildRecorder()
        } catch (e: Exception) {
            Log.w(TAG, "recorder control error (capturing=$recording): ${e.message}")
        }
    }

    private fun rebuildRecorder() {
        val now = System.currentTimeMillis()
        if (now - lastRebuildAttemptMs < rebuildRetryIntervalMs) return
        lastRebuildAttemptMs = now

        effects.forEach { runCatching { it.release() } }
        effects = emptyList()
        recorder?.let { runCatching { it.release() } }
        recorder = null

        try {
            val fresh = createRecorder(currentSource)
            if (fresh.state == AudioRecord.STATE_INITIALIZED) {
                recorder = fresh
                effects = if (unprocessedRequested) disablePreProcessing(fresh.audioSessionId) else emptyList()
                return
            }
            fresh.release()

            // mirror the init fallback to the processed voice recognition source
            if (unprocessedRequested && !isUnprocessedFallback) {
                currentSource = MediaRecorder.AudioSource.VOICE_RECOGNITION
                isUnprocessedFallback = true
                Log.w(TAG, "rebuild: falling back to voice recognition source")
                val fallback = createRecorder(currentSource)
                if (fallback.state == AudioRecord.STATE_INITIALIZED) {
                    recorder = fallback
                    effects = disablePreProcessing(fallback.audioSessionId)
                    return
                }
                fallback.release()
            }

            Log.e(TAG, "recorder rebuild failed to initialize; recorder released, throttled retry, fail-open")
        } catch (e: Exception) {
            Log.e(TAG, "recorder rebuild failed: ${e.message}; throttled retry, fail-open")
        }
    }

    /** Read one chunk from [rec]; null when the read failed or returned no samples. */
    private fun readPacket(rec: AudioRecord): ByteArray? {
        val readCount: Int // number of samples read (for float) or number of bytes read (for int)
        val packetBuffer: ByteArray

        if (audioFormat == AudioFormat.ENCODING_PCM_FLOAT) {
            readCount =
                rec.read(bufferFloat, 0, bufferFloat.size, AudioRecord.READ_BLOCKING)

            if (readCount > 0) {
                bufferFloatConvert.clear()
                bufferFloatConvert.asFloatBuffer().put(bufferFloat, 0, readCount)
                packetBuffer = bufferFloatConvert.array().copyOf(readCount * 4)
            } else {
                packetBuffer = ByteArray(0)
            }
        } else {
            readCount = rec.read(buffer, 0, buffer.size, AudioRecord.READ_BLOCKING)

            if (readCount > 0) {
                packetBuffer = ByteArray(readCount)
                buffer.copyInto(packetBuffer, 0, 0, readCount)
            } else {
                packetBuffer = ByteArray(0)
            }
        }

        return if (readCount > 0) packetBuffer else null
    }

    /** Release the idle recorder and its effects; the next active edge rebuilds cold. */
    private fun releaseIdleRecorder() {
        effects.forEach { runCatching { it.release() } }
        effects = emptyList()
        recorder?.let { runCatching { it.release() } }
        recorder = null
    }

    // shutdown manager
    // should not call any methods after calling
    fun shutdown() {
        streamJob?.cancel()
        runCatching { recorder?.stop() }
        runCatching { recorder?.release() }
        recorder = null
        effects.forEach { runCatching { it.release() } }
        effects = emptyList()
        captureDispatcher.close()
        Log.d(TAG, "shutdown")
    }

    private fun silencePacket(): AudioPacket {
        val buf = silenceBuffer.copyOf()
        return AudioPacket(
            buffer = buf,
            sampleRate = sampleRate,
            audioFormat = audioFormat,
            channelCount = channelCount
        )
    }

    private fun bytesPerSample(format: Int): Int {
        return when (format) {
            AudioFormat.ENCODING_PCM_8BIT -> 1
            AudioFormat.ENCODING_PCM_16BIT -> 2
            AudioFormat.ENCODING_PCM_24BIT_PACKED -> 3
            AudioFormat.ENCODING_PCM_32BIT, AudioFormat.ENCODING_PCM_FLOAT -> 4
            else -> 2
        }
    }

    // 8 bit pcm is unsigned
    private fun silence(format: Int): Byte {
        return if (format == AudioFormat.ENCODING_PCM_8BIT) 0x80.toByte() else 0
    }

    private fun isUnprocessedSource(source: Int): Boolean {
        return Build.VERSION.SDK_INT >= Build.VERSION_CODES.N && source == MediaRecorder.AudioSource.UNPROCESSED
    }

    private fun isUnprocessedSupported(ctx: Context): Boolean {
        val audioManager = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        return audioManager.getProperty(AudioManager.PROPERTY_SUPPORT_AUDIO_SOURCE_UNPROCESSED)
            .toBoolean()
    }

    // some vendors attach gain control, noise suppression or echo cancellation
    // to every input, explicitly turn them off to keep the signal raw
    private fun disablePreProcessing(sessionId: Int): List<AudioEffect> {
        val effects = mutableListOf<AudioEffect>()

        if (AutomaticGainControl.isAvailable()) {
            AutomaticGainControl.create(sessionId)?.let { effects.add(it) }
        }
        if (NoiseSuppressor.isAvailable()) {
            NoiseSuppressor.create(sessionId)?.let { effects.add(it) }
        }
        if (AcousticEchoCanceler.isAvailable()) {
            AcousticEchoCanceler.create(sessionId)?.let { effects.add(it) }
        }

        effects.forEach {
            try {
                it.enabled = false
            } catch (e: IllegalStateException) {
                Log.w(TAG, "failed to disable ${it.javaClass.simpleName}: ${e.message}")
            }
        }

        return effects
    }
}
