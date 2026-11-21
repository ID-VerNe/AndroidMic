package io.github.teamclouday.androidMic.domain.audio

/**
 * Shipped immediate strategy: after the silence keepalive tail ends the recorder is
 * released; the next active edge cold-starts. The 1.5 s tail is owned by the audio
 * manager and passed in as [tailUntilMs]; this strategy keeps the pre-policy baseline
 * semantics, including its recorder retention behaviour after the tail.
 */
class ImmediateHoldStrategy : RecorderHoldStrategy {

    @Volatile
    override var held: Boolean = true
        private set

    @Volatile
    override var reusable: Boolean = true
        private set

    private var active = true

    override fun onActive() {
        val wasHeld = held
        active = true
        held = true
        reusable = wasHeld
    }

    override fun onIdle(now: Long) {
        if (!active) return
        active = false
        held = true
        reusable = true
    }

    override fun evaluate(now: Long, tailUntilMs: Long?): HoldDecision {
        if (active) {
            held = true
            return HoldDecision.Hold(reusable)
        }
        if (tailUntilMs != null && now < tailUntilMs) {
            held = true
            reusable = true
            return HoldDecision.Hold(reusable = true)
        }
        release()
        return HoldDecision.Release
    }

    override fun holdsOpenWhenIdle(): Boolean = false

    override fun releasesRecorderWhenIdle(): Boolean = false

    private fun release() {
        held = false
        reusable = false
    }
}
