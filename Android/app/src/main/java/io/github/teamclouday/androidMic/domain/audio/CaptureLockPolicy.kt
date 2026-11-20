package io.github.teamclouday.androidMic.domain.audio

/**
 * Recorder / microphone lock hold strategy after the on-demand gate stops capturing.
 *
 * The shipped release behaviour is always [IMMEDIATE]; [GRACE_30S] and [ALWAYS] are
 * debug-only experiments for local resume-latency / power measurements (see
 * [LockPolicySelector]). The enum itself is plain JVM so the strategy decisions can be
 * unit tested without an Android runtime.
 */
enum class CaptureLockPolicy {

    /**
     * Shipped behaviour: after the silence keepalive tail ends, the recorder is stopped
     * and its resources are released; the next demand edge cold-starts a fresh recorder.
     * The existing ~1.5 s silence tail (see [MicAudioManager.SILENCE_KEEPALIVE_MS]) is part
     * of this mode and is unchanged.
     */
    IMMEDIATE,

    /**
     * Stop capture but keep the recorder (and the mic lock) alive for [GRACE_WINDOW_MS].
     * A demand edge arriving inside the window resumes on the existing recorder without a
     * rebuild; after the window elapses the recorder is released like [IMMEDIATE].
     */
    GRACE_30S,

    /**
     * Hold the mic / recorder for the whole connection lifetime. Idle capture keeps reading
     * the mic; the gate drops / mutes the audio locally and nothing is sent to the PC.
     * Trades idle power for the lowest possible resume latency.
     */
    ALWAYS;

    companion object {
        /** grace window for [GRACE_30S] after capture goes idle. */
        const val GRACE_WINDOW_MS = 30_000L
    }
}
