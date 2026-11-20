package io.github.teamclouday.androidMic.domain.audio

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CaptureGateTest {

    private var now = 0L
    private val clock = { now }

    @Test
    fun `unlinked gate always captures even when muted or demand false`() {
        val gate = CaptureGate(clock)

        assertFalse(gate.linked)

        // legacy PC: mute must keep the recorder alive (silence fill lives in the manager)
        assertTrue(gate.update(muted = true))
        assertTrue(gate.isCapturing)

        gate.setPcEnabled(false)
        assertTrue(gate.update(muted = false))
        assertTrue(gate.isCapturing)
    }

    @Test
    fun `gate stays unlinked and always-on before a supported hello is processed`() {
        // mirrors an unsupported-protocol hello: the gate must not link, legacy behavior holds
        val gate = CaptureGate(clock)
        assertFalse(gate.linked)

        gate.setPcEnabled(false)
        assertTrue(gate.update(muted = false))
        assertTrue(gate.isCapturing)

        gate.setPcEnabled(true)
        assertTrue(gate.update(muted = true))
        assertTrue(gate.isCapturing)
    }

    @Test
    fun `linked demand and mute matrix`() {
        // demand true, unmuted -> capturing
        var gate = CaptureGate(clock).apply { link(true) }
        assertTrue(gate.update(muted = false))

        // demand true, muted -> mute wins, not capturing
        gate = CaptureGate(clock).apply { link(true) }
        assertFalse(gate.update(muted = true))

        // demand false, unmuted -> stays capturing through the debounce, then stops
        gate = CaptureGate(clock).apply { link(false) }
        assertTrue(gate.update(muted = false))
        now = CaptureGate.OFF_DEBOUNCE_MS
        assertFalse(gate.update(muted = false))

        // demand false, muted -> not capturing
        gate = CaptureGate(clock).apply { link(false) }
        assertFalse(gate.update(muted = true))
    }

    @Test
    fun `off debounce keeps capturing at 299ms and stops at 300ms`() {
        val gate = CaptureGate(clock).apply { link(true) }
        gate.update(muted = false)
        gate.setPcEnabled(false)
        gate.update(muted = false)

        now = 299
        assertTrue(gate.update(muted = false))
        now = 300
        assertFalse(gate.update(muted = false))
    }

    @Test
    fun `re demand cancels a pending off debounce`() {
        val gate = CaptureGate(clock).apply { link(true) }
        gate.update(muted = false)
        gate.setPcEnabled(false)
        gate.update(muted = false)

        now = 100
        gate.setPcEnabled(true)
        assertTrue(gate.update(muted = false))

        // the cancelled deadline must not stop capture later
        now = 500
        assertTrue(gate.update(muted = false))
    }

    @Test
    fun `first three off cycles use the short 300ms debounce`() {
        val gate = CaptureGate(clock).apply { link(false) }

        repeat(2) {
            gate.setPcEnabled(true)
            gate.update(muted = false)
            now += 600
            gate.setPcEnabled(false)
            gate.update(muted = false)
            now += 300
            gate.update(muted = false)
        }

        // third rising edge, then fall without another edge -> 300ms
        gate.setPcEnabled(true)
        gate.update(muted = false)
        now += 600
        gate.setPcEnabled(false)
        gate.update(muted = false)

        now += 299
        assertTrue(gate.update(muted = false))
        now += 1
        assertFalse(gate.update(muted = false))
    }

    @Test
    fun `fourth rising edge switches the off delay to two seconds`() {
        val gate = CaptureGate(clock).apply { link(false) }

        repeat(3) {
            gate.setPcEnabled(true)
            gate.update(muted = false)
            now += 600
            gate.setPcEnabled(false)
            gate.update(muted = false)
            now += 300
            gate.update(muted = false)
        }

        // fourth rising edge in the window -> extended debounce
        gate.setPcEnabled(true)
        gate.update(muted = false)
        now += 600
        gate.setPcEnabled(false)
        gate.update(muted = false)

        now += 1999
        assertTrue(gate.update(muted = false))
        now += 1
        assertFalse(gate.update(muted = false))
    }

    @Test
    fun `five seconds without demand resets the rising edge counter`() {
        val gate = CaptureGate(clock).apply { link(false) }

        repeat(4) {
            gate.setPcEnabled(true)
            gate.update(muted = false)
            now += 100
            gate.setPcEnabled(false)
            gate.update(muted = false)
            now += 100
        }

        // stay without demand for the idle reset window
        now += CaptureGate.IDLE_RESET_MS
        gate.update(muted = false)

        // a fresh cycle counts as the first edge again -> short debounce
        gate.setPcEnabled(true)
        gate.update(muted = false)
        now += 100
        gate.setPcEnabled(false)
        gate.update(muted = false)

        now += 299
        assertTrue(gate.update(muted = false))
        now += 1
        assertFalse(gate.update(muted = false))
    }

    @Test
    fun `reset returns the gate to the unlinked always-on state`() {
        val gate = CaptureGate(clock).apply {
            link(true)
            setPcEnabled(false)
            update(muted = false)
        }
        gate.reset()

        assertFalse(gate.linked)
        assertTrue(gate.update(muted = true))
        assertTrue(gate.isCapturing)
    }
}
