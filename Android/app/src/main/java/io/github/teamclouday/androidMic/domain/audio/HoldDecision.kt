package io.github.teamclouday.androidMic.domain.audio

/** Outcome of a recorder hold decision. Shared seam; no experimental policy names here. */
sealed interface HoldDecision {

    /** Recorder retained; [reusable] means the next active edge can restart it without rebuild. */
    data class Hold(val reusable: Boolean) : HoldDecision

    /** Recorder released; the next active edge must rebuild it cold. */
    data object Release : HoldDecision
}
