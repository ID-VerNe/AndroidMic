package io.github.teamclouday.androidMic.domain.audio

/**
 * On-demand capture gate.
 *
 * Pure JVM state machine (no `android.*` imports) so it can be unit tested headlessly.
 * The capture loop drives it periodically; the state machine itself never blocks.
 *
 * Behaviour:
 * - Before the PC on-demand handshake completes ([linked] == false) the gate reports
 *   capturing unconditionally, matching the legacy always-on behaviour. Muting in that
 *   state keeps the recorder alive; silence filling stays in the audio manager.
 * - After linking, capture follows `pcEnabled && !muted`. Mute takes priority: when muted
 *   the mic is not opened even if the PC still demands audio.
 * - Turning capture off goes through a debounce: 300 ms normally, 2000 ms once the rising
 *   edge count in the sliding window reaches [EXTENDED_EDGE_COUNT]. A continuous 5 s period
 *   without demand resets the edge counter.
 *
 * Note: the phone cannot see the PC-side process PID, so repeated triggers are counted via
 * `pcEnabled` rising edges as the equivalent of "the same PID re-opened the capture stream".
 */
class CaptureGate(
    private val clock: () -> Long = { System.currentTimeMillis() },
) {

    companion object {
        const val OFF_DEBOUNCE_MS = 300L
        const val EXTENDED_OFF_DEBOUNCE_MS = 2000L
        /** from this many rising edges inside the window, the off-delay is extended */
        const val EXTENDED_EDGE_COUNT = 4
        const val RISING_EDGE_WINDOW_MS = 5000L
        /** continuous no-demand period after which the edge counter resets */
        const val IDLE_RESET_MS = 5000L
    }

    /** whether the PC completed the on-demand handshake (Hello processed) */
    var linked: Boolean = false
        private set

    /** latest demand reported by the PC (only meaningful when [linked]) */
    var pcEnabled: Boolean = false
        private set

    /** true while the microphone should be recording */
    var isCapturing: Boolean = true
        private set

    private var previousPcEnabled = false
    private val risingEdges = mutableListOf<Long>()
    private var offDeadlineMs: Long? = null
    private var demandFalseSinceMs: Long? = null

    /** Hello processed: on-demand mode is active. The hello's initial demand is not a rising edge. */
    fun link(initialEnabled: Boolean) = synchronized(this) {
        linked = true
        pcEnabled = initialEnabled
        previousPcEnabled = initialEnabled
        demandFalseSinceMs = if (initialEnabled) null else clock()
        offDeadlineMs = null
        risingEdges.clear()
    }

    /** Latest CaptureControl demand from the PC. */
    fun setPcEnabled(enabled: Boolean) = synchronized(this) {
        val now = clock()
        if (linked) {
            if (enabled && !previousPcEnabled) {
                pruneOldEdges(now)
                risingEdges.add(now)
                demandFalseSinceMs = null
            }
            if (!enabled && previousPcEnabled) {
                demandFalseSinceMs = now
            }
        }
        previousPcEnabled = enabled
        pcEnabled = enabled
    }

    /**
     * Recompute the capture decision for the current mute state.
     * @return [isCapturing]
     */
    fun update(muted: Boolean): Boolean = synchronized(this) {
        val now = clock()

        if (!linked) {
            // legacy PC: always open, mute keeps the recorder alive (silence fill in the manager)
            isCapturing = true
            offDeadlineMs = null
            return true
        }

        pruneOldEdges(now)
        if (!pcEnabled) {
            if (demandFalseSinceMs == null) demandFalseSinceMs = now
            if (now - demandFalseSinceMs!! >= IDLE_RESET_MS) risingEdges.clear()
        } else {
            demandFalseSinceMs = null
        }

        val target = pcEnabled && !muted

        if (target) {
            offDeadlineMs = null
            isCapturing = true
            return true
        }

        // mute wins: stop the mic immediately, no demand off-debounce applies
        if (muted) {
            offDeadlineMs = null
            isCapturing = false
            return false
        }

        // demand == false: start / continue the off debounce
        if (offDeadlineMs == null && isCapturing) {
            val delay = if (risingEdges.size >= EXTENDED_EDGE_COUNT) {
                EXTENDED_OFF_DEBOUNCE_MS
            } else {
                OFF_DEBOUNCE_MS
            }
            offDeadlineMs = now + delay
        }

        val deadline = offDeadlineMs
        isCapturing = when {
            deadline == null -> false
            now < deadline -> true
            else -> {
                offDeadlineMs = null
                false
            }
        }
        return isCapturing
    }

    /** Drop all link/demand state when the stream disconnects. */
    fun reset() = synchronized(this) {
        linked = false
        pcEnabled = false
        previousPcEnabled = false
        risingEdges.clear()
        offDeadlineMs = null
        demandFalseSinceMs = null
        isCapturing = true
    }

    private fun pruneOldEdges(now: Long) {
        risingEdges.removeAll { now - it > RISING_EDGE_WINDOW_MS }
    }
}
