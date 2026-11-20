package io.github.teamclouday.androidMic.domain.streaming

import android.os.Messenger
import io.github.teamclouday.androidMic.domain.service.AudioPacket
import kotlinx.coroutines.flow.Flow

const val CHECK_1 = "AndroidMic1"
const val CHECK_2 = "AndroidMic2"

const val DEFAULT_PORT: Int = 54345

/**
 * Receives on-demand control messages from the PC. Only the TCP/ADB streamer supports it;
 * the default no-op implementation keeps USB / UDP / Bluetooth unaffected.
 */
interface CaptureListener {
    fun onCaptureHello(hello: CaptureMessage.Hello) {}
    fun onCaptureControl(enabled: Boolean) {}

    /**
     * The control channel broke unexpectedly (protocol error or IO failure while the
     * streamer was still supposed to be connected). The gate should fail open back to the
     * unlinked always-on state. Not called for an explicit disconnect.
     */
    fun onCaptureChannelClosed() {}
}

interface Streamer {
    fun connect(): Boolean
    fun disconnect(): Boolean
    fun shutdown()
    fun start(audioStream: Flow<AudioPacket>, tx: Messenger)
    fun getInfo(): String
    fun isAlive(): Boolean

    /** Attach the on-demand control listener. Default: unsupported, ignored. */
    fun setCaptureListener(listener: CaptureListener) {}
}