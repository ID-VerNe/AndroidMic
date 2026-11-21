package io.github.teamclouday.androidMic.domain.audio

/**
 * Experimental recorder / mic lock hold strategies. Debug source set only: these
 * constants and the grace / always behaviours never compile into a release build.
 * The shipped release behaviour is the immediate strategy supplied by main.
 */
enum class CaptureLockPolicy {

    /**
     * Stop reading after the silence keepalive tail; the stopped recorder object is
     * retained and the next demand edge resumes it (`startRecording`) instead of
     * rebuilding a fresh AudioRecord. Matches the shipped immediate behaviour.
     */
    IMMEDIATE,

    /**
     * Keep the recorder (and the mic lock) alive for [GRACE_WINDOW_MS] after the idle
     * edge; a demand edge inside the window resumes on the existing recorder without a
     * rebuild, after which it releases like [IMMEDIATE].
     */
    GRACE_30S,

    /**
     * Hold the mic / recorder for the whole connection lifetime. Idle capture keeps
     * reading the mic; the gate drops / mutes the audio locally and nothing is sent to
     * the PC. Trades idle power for the lowest possible resume latency.
     */
    ALWAYS;

    companion object {
        /** grace window for [GRACE_30S] after capture goes idle. */
        const val GRACE_WINDOW_MS = 30_000L
    }
}
