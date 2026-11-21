package io.github.teamclouday.androidMic.domain.audio

import android.content.Context

/**
 * Debug variant strategy factory. Reads the policy override file once at strategy
 * construction (i.e. when the audio manager / stream starts); an unreadable or
 * unrecognized file resolves to [CaptureLockPolicy.IMMEDIATE]. Debug source set only.
 */
internal fun provideHoldStrategy(
    clock: () -> Long,
    context: Context?,
): RecorderHoldStrategy {
    val policy = LockPolicyOverrideSource.fromContext(context).read()
    return DebugHoldStrategy(RecorderHoldController(policy, clock))
}
