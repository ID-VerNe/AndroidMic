package io.github.teamclouday.androidMic.domain.audio

/**
 * Recorder / mic lock hold strategy for the on-demand capture idle path.
 *
 * Main defines the seam only: the release variant supplies the shipped immediate
 * behaviour, the debug variant supplies the experimental grace / always strategies.
 * The decision logic stays pure JVM so it can be unit tested without an Android runtime.
 */
interface RecorderHoldStrategy {

    /** Recorder / mic lock currently retained (not released). */
    val held: Boolean

    /**
     * Whether the retained recorder can be reused warm on the next active edge.
     * Advisory metadata computed by the strategy; the capture loop decides reuse
     * from whether the recorder object is still retained (`recorder != null`) and
     * does not read this flag directly.
     */
    val reusable: Boolean

    /** Demand edge: the PC wants audio again (or the link opened enabled). */
    fun onActive()

    /** Demand edge: capture went idle. Repeated idle polls are ignored. */
    fun onIdle(now: Long)

    /**
     * Recompute the hold decision for the current instant.
     * @param tailUntilMs silence keepalive tail deadline owned by the audio manager
     *   ([MicAudioManager.SILENCE_KEEPALIVE_MS]); only consulted by the immediate strategy.
     */
    fun evaluate(now: Long, tailUntilMs: Long?): HoldDecision

    /** True: keep the recorder recording while idle and drop packets locally. */
    fun holdsOpenWhenIdle(): Boolean

    /** True: release the idle recorder once the strategy reports [HoldDecision.Release]. */
    fun releasesRecorderWhenIdle(): Boolean
}
