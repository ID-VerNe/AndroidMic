package io.github.teamclouday.androidMic.domain.audio

import android.content.Context

/**
 * Release variant strategy factory. Always resolves to the shipped
 * [ImmediateHoldStrategy]; experimental policy symbols never compile into this
 * source set. The [context] parameter exists only to match the debug factory signature.
 */
internal fun provideHoldStrategy(
    clock: () -> Long,
    context: Context?,
): RecorderHoldStrategy = ImmediateHoldStrategy()
