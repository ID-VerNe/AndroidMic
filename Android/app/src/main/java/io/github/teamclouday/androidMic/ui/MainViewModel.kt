package io.github.teamclouday.androidMic.ui

import android.Manifest
import android.content.pm.PackageManager
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.Message
import android.os.Messenger
import android.os.Process
import android.util.Log
import androidx.compose.runtime.mutableStateOf
import androidx.core.content.ContextCompat
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import io.github.teamclouday.androidMic.AndroidMicApp
import io.github.teamclouday.androidMic.AppPreferences
import io.github.teamclouday.androidMic.AudioFormat
import io.github.teamclouday.androidMic.AudioSource
import io.github.teamclouday.androidMic.ChannelCount
import io.github.teamclouday.androidMic.Dialogs
import io.github.teamclouday.androidMic.Mode
import io.github.teamclouday.androidMic.R
import io.github.teamclouday.androidMic.SampleRates
import io.github.teamclouday.androidMic.Themes
import io.github.teamclouday.androidMic.domain.audio.CaptureGate
import io.github.teamclouday.androidMic.domain.service.AutoConnectPolicy
import io.github.teamclouday.androidMic.domain.service.Command
import io.github.teamclouday.androidMic.domain.service.CommandData
import io.github.teamclouday.androidMic.domain.service.ResponseData
import io.github.teamclouday.androidMic.domain.service.ResponseKind
import io.github.teamclouday.androidMic.domain.streaming.AutoTransport
import io.github.teamclouday.androidMic.ui.utils.UiHelper
import io.github.teamclouday.androidMic.utils.Either
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull

private const val TAG = "MainViewModel"

class MainViewModel : ViewModel() {

    val prefs: AppPreferences = AndroidMicApp.appModule.appPreferences
    val uiHelper: UiHelper = AndroidMicApp.appModule.uiHelper

    private var service: Messenger? = null
    private var isBound = false

    lateinit var handlerThread: HandlerThread
    private lateinit var messenger: Messenger
    lateinit var messengerLooper: Looper


    val textLog = mutableStateOf("")

    val isStreamStarted = mutableStateOf(false)
    val isButtonConnectClickable = mutableStateOf(false)

    val isMuted = mutableStateOf(false)

    /** Live on-demand gate state; drives the status line above the log. */
    val captureState = mutableStateOf(CaptureGate.State.Legacy)

    private var autoConnectJob: Job? = null
    private var autoAttempt: CompletableDeferred<Boolean>? = null
    private var autoConnectPausedLocal = false
    private var pendingAutoConnect = false
    private var explicitAutoConnectRequest = false

    init {
        Log.d(TAG, "init")
    }

    private inner class ReplyHandler(looper: Looper) : Handler(looper) {
        override fun handleMessage(msg: Message) {

            val data = ResponseData.fromMessage(msg)

            when (data.kind) {
                ResponseKind.Standard -> {
                    data.isConnected?.let {
                        isButtonConnectClickable.value = true
                        isStreamStarted.value = it
                        if (!it) {
                            // disconnected: nothing is linked, so back to the legacy label
                            captureState.value = CaptureGate.State.Legacy
                        }
                        autoAttempt?.complete(it)
                        if (!it && pendingAutoConnect) {
                            maybeAutoConnect()
                        }
                    }

                    data.isMuted?.let {
                        isMuted.value = it
                    }

                    data.captureState?.let {
                        captureState.value = it
                    }

                    data.msg?.let {
                        addLogMessage(it)
                    }
                }
            }
        }
    }

    fun handlerServiceResponse() {
        handlerThread =
            HandlerThread("MainViewModelResponseHandler", Process.THREAD_PRIORITY_BACKGROUND)
        handlerThread.start()
        messengerLooper = handlerThread.looper
        messenger = Messenger(ReplyHandler(messengerLooper))
    }

    fun refreshAppVariables() {
        isBound = AndroidMicApp.isBound
        service = AndroidMicApp.service

        isStreamStarted.value = false
        isButtonConnectClickable.value = false
        isMuted.value = false

        // decide once per bind whether this launch should try auto-connect;
        // the actual trigger waits for the BindCheck response below
        pendingAutoConnect = explicitAutoConnectRequest || canAutoConnectByPreferences()

        val msg = CommandData(Command.BindCheck).toCommandMsg()
        msg.replyTo = messenger
        service?.send(msg)
    }

    fun onMuteSwitch() {
        if (!isBound) return

        val message = if (isMuted.value) {
            isMuted.value = false
            CommandData(Command.Unmute)
        } else {
            isMuted.value = true
            CommandData(Command.Mute)
        }.toCommandMsg()

        message.replyTo = messenger
        service?.send(message)
    }

    fun onConnectButton(): Dialogs? {
        if (!isBound) return null
        isMuted.value = false

        // A manual action always wins over a pending/in-flight auto attempt.
        pendingAutoConnect = false
        explicitAutoConnectRequest = false
        cancelAutoConnect()

        val message = if (isStreamStarted.value) {
            Log.d(TAG, "onConnectButton: stop stream")
            // suppress auto-connect until the user explicitly presses Connect
            autoConnectPausedLocal = true
            viewModelScope.launch { prefs.autoConnectPaused.update(true) }
            CommandData(Command.StopStream)
        } else {
            Log.d(TAG, "onConnectButton: start stream")
            autoConnectPausedLocal = false
            viewModelScope.launch { prefs.autoConnectPaused.update(false) }

            val res = runBlocking {
                CommandData.fromPref(prefs, Command.StartStream)
            }
            when (res) {
                is Either.Left<CommandData> -> res.value

                is Either.Right<Dialogs> -> {
                    uiHelper.makeToast(
                        uiHelper.getString(R.string.invalid_ip_port)
                    )
                    return res.value
                }
            }
        }.toCommandMsg()

        // lock button to avoid duplicate events
        isButtonConnectClickable.value = false

        message.replyTo = messenger
        service?.send(message)

        return null
    }

    /**
     * Trigger an auto-connect attempt. Repeated calls while one is running are
     * ignored. Safe to call before the service is bound; it simply returns.
     */
    fun maybeAutoConnect() {
        if (!pendingAutoConnect) return
        if (autoConnectJob?.isActive == true) return
        if (!isBound || isStreamStarted.value || !isButtonConnectClickable.value) return

        autoConnectJob = viewModelScope.launch {
            if (!shouldAutoConnect()) {
                pendingAutoConnect = false
                return@launch
            }

            for (attempt in 0 until AutoConnectPolicy.attemptCount) {
                if (autoConnectPausedLocal || isStreamStarted.value) {
                    pendingAutoConnect = false
                    return@launch
                }
                if (attempt > 0) {
                    delay(AutoConnectPolicy.delayFor(attempt))
                }
                if (!shouldAutoConnect()) {
                    pendingAutoConnect = false
                    return@launch
                }

                val result = attemptStartStream()
                if (result) {
                    pendingAutoConnect = false
                    explicitAutoConnectRequest = false
                    return@launch
                }
                Log.d(TAG, "auto connect attempt ${attempt + 1} failed")
            }

            Log.d(TAG, "auto connect gave up after ${AutoConnectPolicy.attemptCount} attempts")
            pendingAutoConnect = false
        }
    }

    /**
     * Called by MainActivity when it receives an explicit auto-connect intent.
     *
     * The PC also pushes its LAN address and port over the same intent, so an
     * `AUTO` phone knows where to reconnect after the cable is pulled without
     * the user typing an IP. Invalid addresses are ignored rather than stored.
     */
    fun requestAutoConnect(pcIp: String? = null, pcPort: Int? = null) {
        val lanIp = AutoTransport.usableLanIp(pcIp)
        val port = pcPort?.takeIf { it > 0 }

        if (lanIp == null && port == null) {
            explicitAutoConnectRequest = true
            pendingAutoConnect = true
            maybeAutoConnect()
            return
        }

        viewModelScope.launch {
            lanIp?.let { prefs.ip.update(it) }
            port?.let { prefs.port.update(it.toString()) }
            explicitAutoConnectRequest = true
            pendingAutoConnect = true

            // The PC only pushes the endpoint when a cable was just attached, so
            // a running AUTO stream is on Wi-Fi: restart it to move back to the
            // wired path without the user touching anything (AC3). The reply
            // handler re-arms `maybeAutoConnect` once the service reports the
            // stream stopped.
            if (isStreamStarted.value && prefs.mode.get() == Mode.AUTO) {
                Log.d(TAG, "auto: cable attached while streaming, restarting for the USB path")
                val stop = CommandData(Command.StopStream).toCommandMsg()
                stop.replyTo = messenger
                service?.send(stop)
                return@launch
            }

            maybeAutoConnect()
        }
    }

    private suspend fun attemptStartStream(): Boolean {
        if (!isBound) return false

        val res = CommandData.fromPref(prefs, Command.StartStream)
        if (res is Either.Right<Dialogs>) {
            Log.d(TAG, "auto connect: invalid port, not starting")
            return false
        }
        val data = (res as Either.Left<CommandData>).value

        val deferred = CompletableDeferred<Boolean>()
        autoAttempt = deferred

        isMuted.value = false
        isButtonConnectClickable.value = false
        val message = data.toCommandMsg().apply { replyTo = messenger }
        val target = service
        if (target == null) {
            autoAttempt = null
            return false
        }
        try {
            target.send(message)
        } catch (e: Exception) {
            Log.d(TAG, "auto connect: send failed: ${e.message}")
            autoAttempt = null
            return false
        }

        val connected = withTimeoutOrNull(AutoConnectPolicy.ATTEMPT_TIMEOUT_MS) {
            deferred.await()
        } ?: false

        autoAttempt = null
        return connected
    }

    private suspend fun shouldAutoConnect(): Boolean {
        if (!isBound || isStreamStarted.value || !isButtonConnectClickable.value) return false
        if (autoConnectPausedLocal) return false
        val mode = prefs.mode.get()
        if (mode != Mode.ADB && mode != Mode.AUTO) return false
        // A request pushed by the PC is reason enough to start: the switch only
        // governs launches the phone makes on its own.
        if (!explicitAutoConnectRequest && !prefs.autoConnect.get()) return false
        if (prefs.autoConnectPaused.get()) return false
        if (!hasRecordAudioPermission()) {
            Log.d(TAG, "auto connect: RECORD_AUDIO not granted, skipping")
            return false
        }
        return true
    }

    private fun canAutoConnectByPreferences(): Boolean {
        if (autoConnectPausedLocal) return false
        if (!prefs.autoConnect.getBlocking()) return false
        val mode = prefs.mode.getBlocking()
        if (mode != Mode.ADB && mode != Mode.AUTO) return false
        if (prefs.autoConnectPaused.getBlocking()) return false
        return hasRecordAudioPermission()
    }

    private fun hasRecordAudioPermission(): Boolean {
        val context = AndroidMicApp.appModule.context
        return ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.RECORD_AUDIO
        ) == PackageManager.PERMISSION_GRANTED
    }

    private fun cancelAutoConnect() {
        autoConnectJob?.cancel()
        autoConnectJob = null
        autoAttempt?.cancel()
        autoAttempt = null
    }

    fun setIpPort(ip: String, port: String) {
        viewModelScope.launch {
            prefs.ip.update(ip)
            prefs.port.update(port)
        }
    }

    fun setMode(mode: Mode) {
        viewModelScope.launch {
            prefs.mode.update(mode)
            if ((mode == Mode.ADB || mode == Mode.AUTO) && prefs.autoConnect.get()) {
                explicitAutoConnectRequest = true
                pendingAutoConnect = true
                maybeAutoConnect()
            } else {
                pendingAutoConnect = false
                cancelAutoConnect()
            }
        }
    }

    fun setAutoConnect(enabled: Boolean) {
        viewModelScope.launch {
            prefs.autoConnect.update(enabled)
            if (enabled) {
                // turning the switch on is an explicit override of a manual pause
                autoConnectPausedLocal = false
                prefs.autoConnectPaused.update(false)
                explicitAutoConnectRequest = true
                pendingAutoConnect = true
                maybeAutoConnect()
            } else {
                pendingAutoConnect = false
                cancelAutoConnect()
            }
        }
    }

    fun setSampleRate(sampleRate: SampleRates) {
        viewModelScope.launch {
            prefs.sampleRate.update(sampleRate)
        }
    }

    fun setChannelCount(channelCount: ChannelCount) {
        viewModelScope.launch {
            prefs.channelCount.update(channelCount)
        }
    }

    fun setAudioFormat(audioFormat: AudioFormat) {
        viewModelScope.launch {
            prefs.audioFormat.update(audioFormat)
        }
    }

    fun setAudioSource(audioSource: AudioSource) {
        viewModelScope.launch {
            prefs.audioSource.update(audioSource)
        }
    }


    fun setTheme(theme: Themes) {
        viewModelScope.launch {
            prefs.theme.update(theme)
        }
    }

    fun setDynamicColor(dynamicColor: Boolean) {
        viewModelScope.launch {
            prefs.dynamicColor.update(dynamicColor)
        }
    }

    fun cleanLog() {
        textLog.value = ""
    }


    // ask foreground service for current status
    fun askForStatus() {
        if (!isBound) return
        val message = Message.obtain()
        message.what = Command.GetStatus.ordinal
        message.replyTo = messenger
        service?.send(message)
    }


    // helper function to append log message to textview
    private fun addLogMessage(message: String) {
        textLog.value = textLog.value + message + "\n"
    }

    var perms: List<String> = listOf()
    fun showPermissionDialog(perms: List<String>) {
        this.perms = perms
    }
}