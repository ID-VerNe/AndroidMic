package io.github.teamclouday.androidMic.domain.audio

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ImmediateHoldStrategyTest {

    @Test
    fun `holds through the silence tail then releases with zero residual`() {
        val strategy = ImmediateHoldStrategy()
        strategy.onIdle(now = 0L)
        val tailUntil = 1_500L

        val duringTail = strategy.evaluate(now = 1_000L, tailUntil)
        assertTrue(duringTail is HoldDecision.Hold)
        assertTrue(strategy.held)
        assertTrue(strategy.reusable)

        val afterTail = strategy.evaluate(now = 1_500L, tailUntil)
        assertTrue(afterTail is HoldDecision.Release)
        assertFalse(strategy.held)
        assertFalse(strategy.reusable)

        strategy.evaluate(now = 60_000L, tailUntil)
        assertFalse(strategy.held)
        assertFalse(strategy.reusable)
    }

    @Test
    fun `active edge after the tail reports non-reusable at the strategy level`() {
        val strategy = ImmediateHoldStrategy()
        strategy.onIdle(now = 0L)
        strategy.evaluate(now = 5_000L, tailUntilMs = 1_500L)
        assertFalse(strategy.held)

        strategy.onActive()
        assertTrue(strategy.held)
        assertFalse(strategy.reusable)
    }

    @Test
    fun `immediate never holds open idle and never releases the recorder itself`() {
        val strategy = ImmediateHoldStrategy()
        assertFalse(strategy.holdsOpenWhenIdle())
        assertFalse(strategy.releasesRecorderWhenIdle())
    }
}
