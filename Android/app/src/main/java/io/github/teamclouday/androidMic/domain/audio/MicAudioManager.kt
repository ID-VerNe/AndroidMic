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

    private var recorder: AudioRecord
    private var effects: List<AudioEffect> = emptyList()
    private val chunkSize: Int
    private val buffer: ByteArray
    private val bufferFloat: FloatArray
    private val bufferFloatConvert: ByteBuffer
    private var streamJob: Job? = null

    // recorder configuration kept for error recovery rebuilds
    private var channelConfig: Int = 0
    private var bufferSize: Int = 0
    private var minBufferSize: Int = 0
    private var currentSource: Int = audioSource
    private var unprocessedRequested: Boolean = false

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
        require(recorder.state == AudioRecord.STATE_INITIALIZED) {
            "Microphone recording failed to initialize"
        }

        effects = if (unprocessedRequested) disablePreProcessing(recorder.audioSessionId) else emptyList()

        buffer = ByteArray(chunkSize)
        bufferFloat = FloatArray(chunkSize / 4) // float is 4 bytes
        bufferFloatConvert = ByteBuffer.allocate(chunkSize).order(ByteOrder.nativeOrder())

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
                    keepaliveUntilMs = 0L
                    setCapturing(true)

                    if (recorder.state != AudioRecord.STATE_INITIALIZED ||
                        recorder.recordingState != AudioRecord.RECORDSTATE_RECORDING
                    ) {
                        delay(RECORD_DELAY_MS)
                        continue
                    }

                    val readCount: Int // number of samples read (for float) or number of bytes read (for int)
                    val packetBuffer: ByteArray

                    if (audioFormat == AudioFormat.ENCODING_PCM_FLOAT) {
                        readCount =
                            recorder.read(bufferFloat, 0, bufferFloat.size, AudioRecord.READ_BLOCKING)

                        if (readCount > 0) {
                            bufferFloatConvert.clear()
                            bufferFloatConvert.asFloatBuffer().put(bufferFloat, 0, readCount)
                            packetBuffer = bufferFloatConvert.array().copyOf(readCount * 4)
                        } else {
                            packetBuffer = ByteArray(0)
                        }
                    } else {
                        readCount = recorder.read(buffer, 0, buffer.size, AudioRecord.READ_BLOCKING)

                        if (readCount > 0) {
                            packetBuffer = ByteArray(readCount)
                            buffer.copyInto(packetBuffer, 0, 0, readCount)
                        } else {
                            packetBuffer = ByteArray(0)
                        }
                    }

                    if (readCount <= 0) {
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
                    // idle: don't block on read, keep the TCP stream alive
                    setCapturing(false)

                    val now = System.currentTimeMillis()
                    if (wasCapturing) {
                        keepaliveUntilMs = now + SILENCE_KEEPALIVE_MS
                    }

                    if (now < keepaliveUntilMs) {
                        send(silencePacket())
                        delay(CHUNK_MS.toLong())
                    } else {
                        delay(RECORD_DELAY_MS)
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
     * rebuild still fails, log it and fail-open (the loop keeps retrying) instead of going
     * silently mute.
     */
    fun setCapturing(recording: Boolean) {
        try {
            if (recording) {
                if (recorder.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
                    recorder.startRecording()
                }
            } else {
                if (recorder.recordingState == AudioRecord.RECORDSTATE_RECORDING) {
                    recorder.stop()
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
        try {
            effects.forEach { runCatching { it.release() } }
            recorder.release()

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

            Log.e(TAG, "recorder rebuild failed to initialize; fail-open best effort")
        } catch (e: Exception) {
            Log.e(TAG, "recorder rebuild failed: ${e.message}; fail-open best effort")
        }
    }

    // shutdown manager
    // should not call any methods after calling
    fun shutdown() {
        streamJob?.cancel()
        recorder.stop()
        recorder.release()
        effects.forEach { it.release() }
        captureDispatcher.close()
        Log.d(TAG, "shutdown")
    }

    private fun silencePacket(): AudioPacket {
        val buf = ByteArray(chunkSize)
        buf.fill(silence(audioFormat))
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
