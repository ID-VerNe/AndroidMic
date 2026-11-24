package io.github.teamclouday.androidMic.domain.audio

import org.junit.Assert.assertEquals
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

class CaptureGateStateTest {

    private var now = 0L
    private val clock = { now }

    @Test
    fun `state starts as legacy and reports every transition once`() {
        val gate = CaptureGate(clock)
        val seen = mutableListOf<CaptureGate.State>()
        gate.onStateChanged = { seen.add(it) }

        // nothing linked yet: legacy always-on
        gate.update(muted = false)
        assertTrue(seen.isEmpty())

        // hello with demand off -> the gate first runs the off debounce (existing
        // CaptureGate semantics: the shutdown delay also covers the first demand-off),
        // then settles into standby
        gate.link(initialEnabled = false)
        gate.update(muted = false)
        assertEquals(listOf(CaptureGate.State.Capturing), seen)

        now += CaptureGate.OFF_DEBOUNCE_MS
        gate.update(muted = false)
        assertEquals(
            listOf(CaptureGate.State.Capturing, CaptureGate.State.Standby),
            seen,
        )

        // repeated updates without a change must not notify again
        gate.update(muted = false)
        gate.update(muted = false)
        assertEquals(2, seen.size)

        // demand on -> capturing
        gate.setPcEnabled(true)
        gate.update(muted = false)
        assertEquals(CaptureGate.State.Capturing, seen.last())
        assertEquals(3, seen.size)

        // demand off flips back after the debounce window
        gate.setPcEnabled(false)
        gate.update(muted = false)
        assertEquals(CaptureGate.State.Capturing, seen.last())
        now += CaptureGate.OFF_DEBOUNCE_MS
        gate.update(muted = false)
        assertEquals(CaptureGate.State.Standby, seen.last())
    }

    @Test
    fun `mute wins over demand and is published as muted`() {
        val gate = CaptureGate(clock)
        val seen = mutableListOf<CaptureGate.State>()
        gate.onStateChanged = { seen.add(it) }

        gate.link(initialEnabled = true)
        gate.update(muted = false)
        assertEquals(CaptureGate.State.Capturing, gate.state)

        gate.update(muted = true)
        assertEquals(CaptureGate.State.Muted, gate.state)
        assertFalse(gate.isCapturing)
        assertEquals(CaptureGate.State.Muted, seen.last())

        gate.update(muted = false)
        assertEquals(CaptureGate.State.Capturing, seen.last())
    }

    @Test
    fun `reset returns to legacy and republishes`() {
        val gate = CaptureGate(clock)
        gate.link(initialEnabled = false)
        gate.update(muted = false)
        now += CaptureGate.OFF_DEBOUNCE_MS
        gate.update(muted = false)
        assertEquals(CaptureGate.State.Standby, gate.state)

        val seen = mutableListOf<CaptureGate.State>()
        gate.onStateChanged = { seen.add(it) }
        gate.reset()

        assertEquals(CaptureGate.State.Legacy, gate.state)
        assertEquals(listOf(CaptureGate.State.Legacy), seen)
    }

    @Test
    fun `unlinked gate never reports muted`() {
        val gate = CaptureGate(clock)
        val seen = mutableListOf<CaptureGate.State>()
        gate.onStateChanged = { seen.add(it) }

        // legacy PC: mute keeps the recorder alive, so the UI must not claim the
        // mic is closed while it is still capturing
        assertTrue(gate.update(muted = true))
        assertEquals(CaptureGate.State.Legacy, gate.state)
        assertTrue(seen.isEmpty())
    }

    @Test
    fun `reset from muted republishes legacy`() {
        val gate = CaptureGate(clock)
        val seen = mutableListOf<CaptureGate.State>()
        gate.onStateChanged = { seen.add(it) }

        gate.link(initialEnabled = true)
        gate.update(muted = true)
        assertEquals(CaptureGate.State.Muted, gate.state)

        gate.reset()
        assertEquals(CaptureGate.State.Legacy, gate.state)
        assertEquals(listOf(CaptureGate.State.Muted, CaptureGate.State.Legacy), seen)
    }
}
