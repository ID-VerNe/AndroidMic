package io.github.teamclouday.androidMic.domain.streaming

import Message.Messages
import com.google.protobuf.InvalidProtocolBufferException
import io.github.teamclouday.androidMic.utils.toBigEndianU32

/** Decoded on-demand control message sent from the PC after the handshake. */
sealed interface CaptureMessage {

    data class Hello(
        val protocolVersion: Int,
        val capabilities: Int,
        val nonce: Int,
        val captureEnabled: Boolean,
    ) : CaptureMessage

    data class Control(val enabled: Boolean) : CaptureMessage

    companion object {
        const val PROTOCOL_VERSION = 1

        /** capability bit 0: on-demand capture supported by the phone */
        const val CAPABILITY_ON_DEMAND = 1
    }
}

/** Parse a CaptureEnvelope payload (length prefix already stripped). Null if unparseable or empty. */
fun parseCaptureEnvelope(payload: ByteArray): CaptureMessage? {
    val envelope = try {
        Messages.CaptureEnvelope.parseFrom(payload)
    } catch (_: InvalidProtocolBufferException) {
        return null
    }

    return when (envelope.payloadCase) {
        Messages.CaptureEnvelope.PayloadCase.HELLO -> {
            val hello = envelope.hello
            CaptureMessage.Hello(
                protocolVersion = hello.protocolVersion,
                capabilities = hello.capabilities,
                nonce = hello.nonce,
                captureEnabled = hello.captureEnabled,
            )
        }

        Messages.CaptureEnvelope.PayloadCase.CONTROL ->
            CaptureMessage.Control(envelope.control.enabled)

        Messages.CaptureEnvelope.PayloadCase.PAYLOAD_NOT_SET,
        null -> null
    }
}

/** Frame a payload with the 4-byte big-endian length prefix used on the wire. */
fun frameCapturePayload(payload: ByteArray): ByteArray {
    val out = ByteArray(4 + payload.size)
    System.arraycopy(payload.size.toBigEndianU32(), 0, out, 0, 4)
    System.arraycopy(payload, 0, out, 4, payload.size)
    return out
}

/** Read a 4-byte big-endian unsigned 32-bit length from [bytes] at [offset]. */
fun readBigEndianU32(bytes: ByteArray, offset: Int = 0): Int =
    ((bytes[offset].toInt() and 0xFF) shl 24) or
        ((bytes[offset + 1].toInt() and 0xFF) shl 16) or
        ((bytes[offset + 2].toInt() and 0xFF) shl 8) or
        (bytes[offset + 3].toInt() and 0xFF)

/** Build the CaptureHelloAck replied as the first post-handshake frame. */
fun buildCaptureHelloAck(nonce: Int): Messages.CaptureHelloAck =
    Messages.CaptureHelloAck.newBuilder()
        .setProtocolVersion(CaptureMessage.PROTOCOL_VERSION)
        .setCapabilities(CaptureMessage.CAPABILITY_ON_DEMAND)
        .setNonce(nonce)
        .build()

/** An ack matches the hello only when the nonce and protocol version line up. */
fun helloAckMatches(ack: Messages.CaptureHelloAck, nonce: Int): Boolean =
    ack.nonce == nonce && ack.protocolVersion == CaptureMessage.PROTOCOL_VERSION
