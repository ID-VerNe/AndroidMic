package io.github.teamclouday.androidMic.domain.audio

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DebugHoldStrategyTest {

    private fun strategy(policy: CaptureLockPolicy) =
        DebugHoldStrategy(RecorderHoldController(policy, { 0L }))

    @Test
    fun `immediate holds neither open idle nor releases the recorder`() {
        val strategy = strategy(CaptureLockPolicy.IMMEDIATE)
        assertFalse(strategy.holdsOpenWhenIdle())
        assertFalse(strategy.releasesRecorderWhenIdle())
    }

    @Test
    fun `grace30s releases the recorder after its window but does not hold open idle`() {
        val strategy = strategy(CaptureLockPolicy.GRACE_30S)
        assertFalse(strategy.holdsOpenWhenIdle())
        assertTrue(strategy.releasesRecorderWhenIdle())
    }

    @Test
    fun `always holds open idle and does not release the recorder itself`() {
        val strategy = strategy(CaptureLockPolicy.ALWAYS)
        assertTrue(strategy.holdsOpenWhenIdle())
        assertFalse(strategy.releasesRecorderWhenIdle())
    }
}
