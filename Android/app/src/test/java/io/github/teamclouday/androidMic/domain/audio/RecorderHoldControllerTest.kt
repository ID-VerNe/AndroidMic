package io.github.teamclouday.androidMic.domain.audio

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RecorderHoldControllerTest {

    private var now = 0L
    private val clock = { now }

    // region immediate

    @Test
    fun `immediate holds through the silence tail then releases with zero residual`() {
        val controller = RecorderHoldController(CaptureLockPolicy.IMMEDIATE, clock)
        controller.onDemandIdle()
        val tailUntil = 1_500L

        // still inside the keepalive tail: recorder retained
        now = 1_000
        val duringTail = controller.update(tailUntil)
        assertTrue(duringTail is HoldDecision.Hold)
        assertTrue(controller.held)
        assertTrue(controller.reusable)

        // tail just ended: released, no warm residual
        now = 1_500
        val afterTail = controller.update(tailUntil)
        assertTrue(afterTail is HoldDecision.Release)
        assertFalse(controller.held)
        assertFalse(controller.reusable)

        // later polls stay released: zero residual after the tail
        now = 60_000
        assertTrue(controller.update(tailUntil) is HoldDecision.Release)
        assertFalse(controller.held)
        assertFalse(controller.reusable)
    }

    @Test
    fun `immediate re-enable after release needs a cold rebuild`() {
        val controller = RecorderHoldController(CaptureLockPolicy.IMMEDIATE, clock)
        controller.onDemandIdle()
        controller.update(1_500L)
        now = 5_000
        controller.update(1_500L)
        assertFalse(controller.held)

        controller.onDemandActive()
        assertTrue(controller.held)
        assertFalse(controller.reusable)
    }

    // endregion

    // region grace 30s

    @Test
    fun `grace keeps the recorder warm inside the 30s window`() {
        val controller = RecorderHoldController(CaptureLockPolicy.GRACE_30S, clock)
        controller.onDemandIdle()

        now = 29_999
        val decision = controller.update()
        assertTrue(decision is HoldDecision.Hold)
        assertTrue(controller.held)
        assertTrue(controller.reusable)
    }

    @Test
    fun `grace releases at the 30s window boundary`() {
        val controller = RecorderHoldController(CaptureLockPolicy.GRACE_30S, clock)
        controller.onDemandIdle()

        now = CaptureLockPolicy.GRACE_WINDOW_MS
        val decision = controller.update()
        assertTrue(decision is HoldDecision.Release)
        assertFalse(controller.held)
        assertFalse(controller.reusable)
    }

    @Test
    fun `grace re-enable inside the window resumes without rebuild`() {
        val controller = RecorderHoldController(CaptureLockPolicy.GRACE_30S, clock)
        controller.onDemandIdle()

        now = 10_000
        controller.onDemandActive()

        assertTrue(controller.held)
        assertTrue(controller.reusable)
        assertTrue(controller.update() is HoldDecision.Hold)
    }

    @Test
    fun `grace re-enable after expiry needs a cold rebuild`() {
        val controller = RecorderHoldController(CaptureLockPolicy.GRACE_30S, clock)
        controller.onDemandIdle()

        now = 30_001
        controller.update()
        assertFalse(controller.held)

        controller.onDemandActive()
        assertTrue(controller.held)
        assertFalse(controller.reusable)
    }

    @Test
    fun `grace idle polls do not slide the window`() {
        val controller = RecorderHoldController(CaptureLockPolicy.GRACE_30S, clock)
        controller.onDemandIdle()

        now = 20_000
        controller.onDemandIdle() // repeated idle poll must not extend the window
        now = 30_001
        assertTrue(controller.update() is HoldDecision.Release)
    }

    // endregion

    // region always

    @Test
    fun `always keeps holding the mic and recorder while idle`() {
        val controller = RecorderHoldController(CaptureLockPolicy.ALWAYS, clock)
        controller.onDemandIdle()

        now = 60_000
        controller.update()
        assertTrue(controller.held)
        assertTrue(controller.reusable)

        now = 3_600_000
        val decision = controller.update()
        assertTrue(decision is HoldDecision.Hold)
        assertTrue(controller.held)
        assertTrue(controller.reusable)
    }

    // endregion

    @Test
    fun `reset returns the controller to the retained active state`() {
        val controller = RecorderHoldController(CaptureLockPolicy.GRACE_30S, clock)
        controller.onDemandIdle()
        now = 60_000
        controller.update()
        assertFalse(controller.held)

        controller.reset()
        assertTrue(controller.held)
        assertTrue(controller.reusable)
        assertTrue(controller.update() is HoldDecision.Hold)
    }
}
