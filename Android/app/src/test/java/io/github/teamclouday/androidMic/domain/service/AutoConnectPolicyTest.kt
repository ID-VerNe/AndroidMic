package io.github.teamclouday.androidMic.domain.service

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AutoConnectPolicyTest {

    @Test
    fun `attempts start immediately then back off and stay bounded`() {
        assertEquals(6, AutoConnectPolicy.attemptCount)
        assertEquals(0L, AutoConnectPolicy.delayFor(0))
        assertEquals(1_000L, AutoConnectPolicy.delayFor(1))
        assertEquals(2_000L, AutoConnectPolicy.delayFor(2))
        assertEquals(4_000L, AutoConnectPolicy.delayFor(3))
        assertEquals(8_000L, AutoConnectPolicy.delayFor(4))
        assertEquals(15_000L, AutoConnectPolicy.delayFor(5))
    }

    @Test
    fun `out of range attempt index is clamped to the valid range`() {
        assertEquals(0L, AutoConnectPolicy.delayFor(-1))
        assertEquals(15_000L, AutoConnectPolicy.delayFor(99))
    }

    @Test
    fun `attempt bounds are exclusive`() {
        assertTrue(AutoConnectPolicy.hasAttempt(0))
        assertTrue(AutoConnectPolicy.hasAttempt(5))
        assertFalse(AutoConnectPolicy.hasAttempt(-1))
        assertFalse(AutoConnectPolicy.hasAttempt(6))
    }

    @Test
    fun `total retry window is about thirty seconds`() {
        val total = AutoConnectPolicy.RETRY_DELAYS_MS.sum()
        assertEquals(30_000L, total)
    }
}
