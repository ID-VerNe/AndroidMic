package io.github.teamclouday.androidMic.domain.audio

import android.content.Context

/**
 * Nightly variant strategy factory. Nightly is a release-like pre-release build
 * (`initWith(release)`), but AGP does not inherit `src/release` java sources into
 * the nightly source set, so it needs its own seam factory. Like release, it
 * always resolves to the shipped [ImmediateHoldStrategy]; experimental policy
 * symbols never compile here. The [context] parameter exists only to match the
 * debug/release factory signature.
 */
internal fun provideHoldStrategy(
    clock: () -> Long,
    context: Context?,
): RecorderHoldStrategy = ImmediateHoldStrategy()
