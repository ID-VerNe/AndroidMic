package io.github.teamclouday.androidMic.domain.streaming

import Message.Messages
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CaptureProtocolTest {

    @Test
    fun `hello envelope parses back to the original fields`() {
        val hello = Messages.CaptureHello.newBuilder()
            .setProtocolVersion(CaptureMessage.PROTOCOL_VERSION)
            .setCapabilities(CaptureMessage.CAPABILITY_ON_DEMAND)
            .setNonce(42)
            .setCaptureEnabled(true)
            .build()
        val envelope = Messages.CaptureEnvelope.newBuilder().setHello(hello).build()

        val parsed = parseCaptureEnvelope(envelope.toByteArray()) as CaptureMessage.Hello
        assertEquals(CaptureMessage.PROTOCOL_VERSION, parsed.protocolVersion)
        assertEquals(CaptureMessage.CAPABILITY_ON_DEMAND, parsed.capabilities)
        assertEquals(42, parsed.nonce)
        assertTrue(parsed.captureEnabled)
    }

    @Test
    fun `control envelope parses the enabled flag`() {
        val envelope = Messages.CaptureEnvelope.newBuilder()
            .setControl(Messages.CaptureControl.newBuilder().setEnabled(false).build())
            .build()

        val parsed = parseCaptureEnvelope(envelope.toByteArray()) as CaptureMessage.Control
        assertFalse(parsed.enabled)
    }

    @Test
    fun `length prefix frames payload and decodes back`() {
        val payload = byteArrayOf(1, 2, 3, 127, -128)
        val frame = frameCapturePayload(payload)

        assertEquals(4 + payload.size, frame.size)
        assertEquals(payload.size, readBigEndianU32(frame))
        assertArrayEquals(payload, frame.copyOfRange(4, frame.size))
    }

    @Test
    fun `ack nonce matches the hello nonce and rejects a mismatch`() {
        val ack = buildCaptureHelloAck(42)

        assertTrue(helloAckMatches(ack, 42))
        assertFalse(helloAckMatches(ack, 43))
    }

    @Test
    fun `garbage payload parses to null`() {
        assertNull(parseCaptureEnvelope(byteArrayOf(1, 2, 3, 4, 5)))
    }

    @Test
    fun `empty payload parses to null`() {
        assertNull(parseCaptureEnvelope(ByteArray(0)))
    }
}
