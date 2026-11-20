package io.github.teamclouday.androidMic.domain.streaming

import Message.Messages
import android.content.Context
import android.net.ConnectivityManager
import android.os.Messenger
import android.util.Log
import com.google.protobuf.ByteString
import io.github.teamclouday.androidMic.domain.service.AudioPacket
import io.github.teamclouday.androidMic.domain.service.Command
import io.github.teamclouday.androidMic.domain.service.CommandData
import io.github.teamclouday.androidMic.utils.toBigEndianU32
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.io.IOException
import java.io.InputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException

class TcpStreamer(
    private val scope: CoroutineScope,
    private val tag: String,
    private val ip: String,
    private var port: Int
) : Streamer {

    private var socket: Socket? = null
    private var streamJob: Job? = null
    private var readerJob: Job? = null

    // serializes every write on the socket (audio frames and the hello ack) so two frames
    // can't interleave and corrupt the length prefix
    private val writeLock = Any()
    private var captureListener: CaptureListener? = null

    companion object {

        fun wifi(
            ctx: Context,
            scope: CoroutineScope,
            ip: String,
            port: Int
        ): TcpStreamer {

            // check WIFI
            // reference: https://stackoverflow.com/questions/70107145/connectivity-manager-allnetworks-deprecated
            val cm = ctx.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
            val net = cm.activeNetwork
            require(net != null) {
                "Wifi not available"
            }
            require(cm.getNetworkCapabilities(net) != null) {
                "Wifi not available"
            }

            return TcpStreamer(
                scope = scope,
                tag = "WifiStreamer",
                ip = ip,
                port = port
            )
        }

        fun adb(
            scope: CoroutineScope,
            port: Int
        ) = TcpStreamer(
            scope = scope,
            tag = "AdbStreamer",
            ip = "127.0.0.1",
            port = port
        )
    }

    override fun setCaptureListener(listener: CaptureListener) {
        captureListener = listener
    }

    // connect to server
    override fun connect(): Boolean {

        val p = port
        val socket = createSocket(p, 1500) ?: return false

        if (!handShake(socket)) {
            Log.d(tag, "connect [Socket]: handshake error")
            socket.close()
            return false
        }

        this.socket = socket
        return true
    }

    // stream data through socket
    override fun start(audioStream: Flow<AudioPacket>, tx: Messenger) {
        streamJob?.cancel()
        startReader()

        streamJob = scope.launch {
            audioStream.collect { data ->
                if (socket == null || socket?.isConnected != true) return@collect

                try {
                    val message = Messages.AudioPacketMessage.newBuilder()
                        .setBuffer(ByteString.copyFrom(data.buffer))
                        .setSampleRate(data.sampleRate)
                        .setAudioFormat(data.audioFormat)
                        .setChannelCount(data.channelCount)
                        .build()
                    val pack = message.toByteArray()

                    // one write per packet, a separate length write would wait on an ack
                    val out = ByteArray(4 + pack.size)
                    System.arraycopy(pack.size.toBigEndianU32(), 0, out, 0, 4)
                    System.arraycopy(pack, 0, out, 4, pack.size)

                    synchronized(writeLock) {
                        socket!!.outputStream.write(out)
                        socket!!.outputStream.flush()
                    }
                } catch (e: IOException) {
                    Log.d(tag, "${e.message}")
                    delay(5)
                    disconnect()
                    tx.send(CommandData(Command.StopStream).toCommandMsg())
                } catch (e: Exception) {
                    Log.d(tag, "${e.message}")
                }
            }
        }
    }

    // disconnect from server
    override fun disconnect(): Boolean {
        if (socket == null) return false
        try {
            socket?.close()
        } catch (e: IOException) {
            Log.d(tag, "disconnect [close]: ${e.message}")
            socket = null
            readerJob?.cancel()
            readerJob = null
            streamJob?.cancel()
            streamJob = null
            return false
        }
        socket = null
        readerJob?.cancel()
        readerJob = null
        streamJob?.cancel()
        streamJob = null
        Log.d(tag, "disconnect: complete")
        return true
    }

    // shutdown streamer
    override fun shutdown() {
        disconnect()
    }

    // get connected server information
    override fun getInfo(): String {
        if (socket == null) return ""
        return "[Device Address]:${socket?.remoteSocketAddress}"
    }

    // return true if is connected for streaming
    override fun isAlive(): Boolean {
        return (socket != null && socket?.isConnected == true)
    }


    fun createSocket(p: Int, timeout: Int): Socket? {
        val socket = Socket()
        return try {
            // send small audio packets right away instead of batching them (Nagle)
            socket.tcpNoDelay = true
            socket.connect(InetSocketAddress(ip, p), timeout)
            socket.soTimeout = timeout
            socket
        } catch (e: IOException) {
            Log.d(tag, "connect [Socket]: ${e.message}")
            null
        } catch (e: SocketTimeoutException) {
            Log.d(tag, "connect [Socket]: ${e.message}")
            null
        }
    }

    fun handShake(
        socket: Socket,
    ): Boolean {

        return try {
            val out = socket.getOutputStream()
            out.write(CHECK_1.toByteArray())
            out.flush()

            val input = socket.getInputStream()
            val msgBuf = ByteArray(CHECK_2.length)
            input.read(msgBuf)
            msgBuf.contentEquals(CHECK_2.toByteArray())

        } catch (_: Exception) {
            false
        }
    }

    /**
     * Reader coroutine for PC -> phone control frames (4-byte big-endian length +
     * CaptureEnvelope). Started after the handshake; only TCP/ADB carry on-demand control.
     */
    private fun startReader() {
        readerJob?.cancel()
        val socket = socket ?: return

        try {
            // blocking read after the handshake; closing the socket unblocks the read and ends the loop
            socket.soTimeout = 0
        } catch (e: Exception) {
            Log.d(tag, "reader: soTimeout reset failed: ${e.message}")
        }

        readerJob = scope.launch {
            val input = socket.inputStream
            var failure: String? = null
            while (isActive) {
                val lenBuf = ByteArray(4)
                if (!readFully(input, lenBuf)) {
                    failure = "read failed or end of stream"
                    break
                }

                val len = readBigEndianU32(lenBuf)
                if (!isValidControlFrameLength(len)) {
                    failure = "unsupported control frame length $len"
                    Log.d(tag, "reader: ${failure}")
                    break
                }

                val payload = ByteArray(len)
                if (!readFully(input, payload)) {
                    failure = "read failed or end of stream"
                    break
                }

                val message = parseCaptureEnvelope(payload) ?: continue
                when (message) {
                    is CaptureMessage.Hello -> handleHello(message)
                    is CaptureMessage.Control -> captureListener?.onCaptureControl(message.enabled)
                }
            }

            // An explicit disconnect nulls the socket; if it is still set the reader broke
            // unexpectedly, so fail the control channel open instead of freezing the gate
            // at its last state.
            if (failure != null && this@TcpStreamer.socket != null) {
                Log.w(tag, "reader: control channel closed unexpectedly (${failure}); failing open")
                captureListener?.onCaptureChannelClosed()
            }
            Log.d(tag, "reader: stopped")
        }
    }

    private fun handleHello(hello: CaptureMessage.Hello) {
        if (!isSupportedProtocolVersion(hello.protocolVersion)) {
            Log.d(tag, "unsupported hello protocol version ${hello.protocolVersion}, staying always-on")
            return
        }

        val ack = buildCaptureHelloAck(hello.nonce)
        try {
            writeFramed(ack.toByteArray())
        } catch (e: IOException) {
            Log.d(tag, "ack write failed: ${e.message}")
            return
        }

        captureListener?.onCaptureHello(hello)
    }

    private fun writeFramed(payload: ByteArray) {
        val socket = socket ?: return
        synchronized(writeLock) {
            val out = frameCapturePayload(payload)
            socket.outputStream.write(out)
            socket.outputStream.flush()
        }
    }

    private fun readFully(input: InputStream, buf: ByteArray): Boolean {
        var total = 0
        while (total < buf.size) {
            val n = try {
                input.read(buf, total, buf.size - total)
            } catch (e: IOException) {
                Log.d(tag, "reader: read failed: ${e.message}")
                return false
            }
            if (n < 0) return false
            total += n
        }
        return true
    }
}
