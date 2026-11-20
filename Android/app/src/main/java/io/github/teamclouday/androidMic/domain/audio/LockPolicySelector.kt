package io.github.teamclouday.androidMic.domain.audio

import io.github.teamclouday.androidMic.BuildConfig

/**
 * Resolves the active [CaptureLockPolicy].
 *
 * Release builds always resolve to [CaptureLockPolicy.IMMEDIATE]. The grace / always
 * strategies are debug-only experiments wired through [DebugLockPolicy.override], which is
 * read only under [BuildConfig.DEBUG]. No UI, preference key, or string resource exposes
 * the switch: it is a code-level override for local latency / power measurements on debug
 * builds.
 */
internal object LockPolicySelector {

    val policy: CaptureLockPolicy =
        if (BuildConfig.DEBUG) {
            DebugLockPolicy.override ?: CaptureLockPolicy.IMMEDIATE
        } else {
            CaptureLockPolicy.IMMEDIATE
        }
}

/**
 * Debug-only manual policy override. Never read on release builds; null keeps
 * [CaptureLockPolicy.IMMEDIATE] even in debug.
 */
internal object DebugLockPolicy {
    var override: CaptureLockPolicy? = null
}
