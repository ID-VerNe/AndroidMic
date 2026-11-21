package io.github.teamclouday.androidMic.domain.audio

/**
 * Debug-only [RecorderHoldStrategy] wrapping the three-policy [RecorderHoldController].
 * The policy is resolved from the debug override file at construction. This class never
 * compiles into a release build.
 */
class DebugHoldStrategy(
    private val controller: RecorderHoldController,
) : RecorderHoldStrategy {

    override val held: Boolean get() = controller.held

    override val reusable: Boolean get() = controller.reusable

    override fun onActive() = controller.onDemandActive()

    override fun onIdle(now: Long) = controller.onDemandIdle()

    override fun evaluate(now: Long, tailUntilMs: Long?): HoldDecision =
        controller.update(tailUntilMs)

    override fun holdsOpenWhenIdle(): Boolean =
        controller.policy == CaptureLockPolicy.ALWAYS

    override fun releasesRecorderWhenIdle(): Boolean =
        controller.policy == CaptureLockPolicy.GRACE_30S
}
