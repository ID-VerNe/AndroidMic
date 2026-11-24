package io.github.teamclouday.androidMic.domain.streaming

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AutoTransportTest {

    @Test
    fun loopbackIsTriedFirstThenTheLanAddress() {
        assertEquals(
            listOf(AutoTransport.LOOPBACK, "192.168.1.7"),
            AutoTransport.candidates("192.168.1.7")
        )
    }

    @Test
    fun blankLanAddressOnlyLeavesLoopback() {
        // Documented "USB only" state: no fallback configured.
        assertEquals(listOf(AutoTransport.LOOPBACK), AutoTransport.candidates(null))
        assertEquals(listOf(AutoTransport.LOOPBACK), AutoTransport.candidates(""))
        assertEquals(listOf(AutoTransport.LOOPBACK), AutoTransport.candidates("   "))
    }

    @Test
    fun ipv4DefaultPrefixIsNotAUsableFallback() {
        // The stored preference default is `192.168.`; `checkIp` accepts it but
        // no socket can connect to it, so it must not become a candidate.
        assertNull(AutoTransport.usableLanIp("192.168."))
        assertNull(AutoTransport.usableLanIp("192.168.1"))
        assertNull(AutoTransport.usableLanIp("192.168.1.7.9"))
        assertNull(AutoTransport.usableLanIp("999.1.1.1"))
    }

    @Test
    fun loopbackIsNotALanFallback() {
        assertNull(AutoTransport.usableLanIp(AutoTransport.LOOPBACK))
        assertEquals(listOf(AutoTransport.LOOPBACK), AutoTransport.candidates(AutoTransport.LOOPBACK))
    }

    @Test
    fun validLanAddressIsAcceptedAndTrimmed() {
        assertEquals("192.168.3.5", AutoTransport.usableLanIp(" 192.168.3.5 "))
        assertEquals("10.0.0.12", AutoTransport.usableLanIp("10.0.0.12"))
    }

    @Test
    fun candidatesNeverRepeatAnAddress() {
        val candidates = AutoTransport.candidates("192.168.1.7")
        assertEquals(candidates.distinct(), candidates)
    }

    @Test
    fun reconnectWindowStaysUnderTheAcceptanceBudget() {
        // AC2 wants audio back within 10 s of the cable being pulled.
        assertTrue(AutoTransport.RECONNECT_WINDOW_MS < 10_000)
        assertTrue(AutoTransport.RECONNECT_DELAY_MS > 0)
        assertTrue(AutoTransport.CONNECT_TIMEOUT_MS > 0)
    }
}
