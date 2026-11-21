package io.github.teamclouday.androidMic.domain.audio

/**
 * Pure-JVM recorder-hold decision machine for the experimental debug policies.
 *
 * Mirrors [CaptureGate]: no `android.*` imports, clock injected, all mutations
 * synchronized. The capture loop reports demand edges; [update] answers whether the
 * recorder / mic lock should stay held and whether the retained recorder can be reused
 * warm on the next active edge.
 *
 * Debug source set only; release builds use [ImmediateHoldStrategy] instead.
 */
class RecorderHoldController(
    val policy: CaptureLockPolicy,
    private val clock: () -> Long = { System.currentTimeMillis() },
) {

    /** Recorder / mic lock currently retained (not released). */
    @Volatile
    var held: Boolean = true
        private set

    /** Retained recorder can be reused on the next active edge without rebuild. */
    @Volatile
    var reusable: Boolean = true
        private set

    private var demandActive = true
    private var graceUntilMs: Long? = null

    /** Demand edge: the PC wants audio again (or the link opened enabled). */
    fun onDemandActive() = synchronized(this) {
        val wasHeld = held
        demandActive = true
        graceUntilMs = null
        held = true
        // If the recorder was already released, the active edge needs a cold rebuild;
        // only a still-retained recorder is reusable warm.
        reusable = wasHeld
    }

    /** Demand edge: the PC dropped audio (capture went idle). Repeated idle polls are ignored. */
    fun onDemandIdle(): Unit = synchronized(this) {
        if (!demandActive) return
        demandActive = false
        val now = clock()
        when (policy) {
            CaptureLockPolicy.IMMEDIATE -> {
                graceUntilMs = null
                held = true
                reusable = true
            }

            CaptureLockPolicy.GRACE_30S -> {
                graceUntilMs = now + CaptureLockPolicy.GRACE_WINDOW_MS
                held = true
                reusable = true
            }

            CaptureLockPolicy.ALWAYS -> {
                graceUntilMs = null
                held = true
                reusable = true
            }
        }
    }

    /**
     * Recompute the hold decision for the current instant.
     *
     * @param tailUntilMs silence keepalive tail deadline owned by the audio manager
     *   ([MicAudioManager.SILENCE_KEEPALIVE_MS]); only consulted by
     *   [CaptureLockPolicy.IMMEDIATE].
     */
    fun update(tailUntilMs: Long? = null): HoldDecision = synchronized(this) {
        val now = clock()
        if (demandActive) {
            held = true
            return HoldDecision.Hold(reusable)
        }

        when (policy) {
            CaptureLockPolicy.ALWAYS -> {
                held = true
                reusable = true
                HoldDecision.Hold(reusable = true)
            }

            CaptureLockPolicy.GRACE_30S -> {
                val until = graceUntilMs
                if (until != null && now < until) {
                    held = true
                    reusable = true
                    HoldDecision.Hold(reusable = true)
                } else {
                    release()
                    HoldDecision.Release
                }
            }

            CaptureLockPolicy.IMMEDIATE -> {
                if (tailUntilMs != null && now < tailUntilMs) {
                    held = true
                    reusable = true
                    HoldDecision.Hold(reusable = true)
                } else {
                    release()
                    HoldDecision.Release
                }
            }
        }
    }

    /**
     * Drop all hold state after a disconnect / reset. Not wired to the production
     * path: the audio manager (and this controller) is rebuilt per connection, so
     * there is no cross-connection state to clear. Kept for tests and a future
     * symmetric teardown if the controller ever outlives a connection.
     */
    fun reset() = synchronized(this) {
        demandActive = true
        graceUntilMs = null
        held = true
        reusable = true
    }

    private fun release() {
        held = false
        reusable = false
        graceUntilMs = null
    }
}
