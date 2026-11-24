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
 *
 * Threading: all mutations happen under `synchronized(this)`. The published state
 * ([linked], [pcEnabled], [isCapturing]) is `@Volatile` so the capture loop can read it
 * off-lock without missing a writer's update. Mute while linked cancels any pending
 * demand-off debounce: mute wins immediately rather than waiting out the off-delay.
 */
class CaptureGate(
    private val clock: () -> Long = { System.currentTimeMillis() },
) {

    /**
     * Coarse state of the capture gate, derived from [linked], [pcEnabled],
     * [isCapturing] and the mute flag. This is what the UI shows: before #41 the
     * app screen kept saying "microphone has started to record" even while the
     * gate held the mic closed.
     */
    enum class State {
        /** Legacy PC (no handshake): always-on capture, same as before the feature. */
        Legacy,

        /** Linked, no local demand: the microphone is closed on purpose. */
        Standby,

        /** Linked and the microphone is open. */
        Capturing,

        /** Linked but the user muted the phone: mic stays closed regardless of demand. */
        Muted,
    }

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
    @Volatile
    var linked: Boolean = false
        private set

    /** latest demand reported by the PC (only meaningful when [linked]) */
    @Volatile
    var pcEnabled: Boolean = false
        private set

    /** true while the microphone should be recording */
    @Volatile
    var isCapturing: Boolean = true
        private set

    /** Latest derived [State]; published on every mutation, only when it changes. */
    @Volatile
    var state: State = State.Legacy
        private set

    private var muted = false

    /**
     * Invoked whenever [state] changes. Runs under the gate lock, so the callback
     * must not call back into the gate.
     *
     * Assigned from the service thread and read from the capture loop, hence
     * `@Volatile` even though the reference is set before the loop starts.
     */
    @Volatile
    var onStateChanged: ((State) -> Unit)? = null

    private var previousPcEnabled = false
    private val risingEdges = mutableListOf<Long>()
    private var offDeadlineMs: Long? = null
    private var demandFalseSinceMs: Long? = null

    /**
     * Hello processed: on-demand mode is active. The hello's initial demand is not a rising edge.
     *
     * The published [state] is not recomputed here: the capture loop calls [update]
     * right after the handshake, and only that call knows the current mute flag.
     */
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
        this.muted = muted
        val now = clock()

        if (!linked) {
            // legacy PC: always open, mute keeps the recorder alive (silence fill in the manager)
            isCapturing = true
            offDeadlineMs = null
            publish()
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

        isCapturing = when {
            target -> {
                offDeadlineMs = null
                true
            }
            // mute wins: stop the mic immediately, no demand off-debounce applies
            muted -> {
                offDeadlineMs = null
                false
            }
            else -> {
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
                if (deadline == null) {
                    false
                } else if (now < deadline) {
                    true
                } else {
                    offDeadlineMs = null
                    false
                }
            }
        }

        publish()
        isCapturing
    }

    /** Derive [state] from the published fields and notify on change. */
    private fun publish() {
        val next = when {
            !linked -> State.Legacy
            muted -> State.Muted
            isCapturing -> State.Capturing
            else -> State.Standby
        }
        if (next == state) return
        state = next
        onStateChanged?.invoke(next)
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
        muted = false
        publish()
    }

    private fun pruneOldEdges(now: Long) {
        risingEdges.removeAll { now - it > RISING_EDGE_WINDOW_MS }
    }
}
