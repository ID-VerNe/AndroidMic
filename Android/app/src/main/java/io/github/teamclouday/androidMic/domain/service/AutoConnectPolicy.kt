package io.github.teamclouday.androidMic.domain.service

/**
 * Bounded retry policy for the Android side auto-connect.
 *
 * The first attempt runs immediately; later attempts back off so a PC that is
 * still creating the `adb reverse` proxy gets a few seconds to catch up.
 * Keeping this separate from the ViewModel makes the sequence unit-testable.
 */
object AutoConnectPolicy {
    val RETRY_DELAYS_MS = longArrayOf(0L, 1_000L, 2_000L, 4_000L, 8_000L, 15_000L)

    /** how long one StartStream attempt may take to produce a response */
    const val ATTEMPT_TIMEOUT_MS = 5_000L

    val attemptCount: Int
        get() = RETRY_DELAYS_MS.size

    fun hasAttempt(attempt: Int): Boolean = attempt in RETRY_DELAYS_MS.indices

    fun delayFor(attempt: Int): Long =
        RETRY_DELAYS_MS[attempt.coerceIn(RETRY_DELAYS_MS.indices)]
}
