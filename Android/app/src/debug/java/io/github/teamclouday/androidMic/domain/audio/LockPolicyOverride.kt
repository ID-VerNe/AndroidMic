package io.github.teamclouday.androidMic.domain.audio

import android.content.Context
import java.io.File

/**
 * Debug-only runtime override for the experimental hold policy. The file lives at
 * `<externalFilesDir>/lock_policy_override.txt` and holds one of `immediate`,
 * `grace30s`, `always` (case-insensitive, trimmed). A missing, empty or unrecognized
 * file falls back to [CaptureLockPolicy.IMMEDIATE]. Debug source set only.
 */
object LockPolicyOverrideParser {

    fun parse(text: String?): CaptureLockPolicy = when (text?.trim()?.lowercase()) {
        "immediate" -> CaptureLockPolicy.IMMEDIATE
        "grace30s" -> CaptureLockPolicy.GRACE_30S
        "always" -> CaptureLockPolicy.ALWAYS
        else -> CaptureLockPolicy.IMMEDIATE
    }
}

/** Reads the override body through an injectable reader so parsing stays pure-JVM testable. */
class LockPolicyOverrideSource(
    private val readText: () -> String?,
) {

    fun read(): CaptureLockPolicy = LockPolicyOverrideParser.parse(readText())

    companion object {
        const val FILE_NAME = "lock_policy_override.txt"

        fun fromContext(context: Context?): LockPolicyOverrideSource =
            fromFile(context?.getExternalFilesDir(null)?.resolve(FILE_NAME))

        /**
         * Wraps a file whose missing state or read failure resolves to `null`
         * (then to [CaptureLockPolicy.IMMEDIATE]). Internal so the file/exception
         * branch can be exercised from the pure-JVM unit tests.
         */
        internal fun fromFile(file: File?): LockPolicyOverrideSource = LockPolicyOverrideSource {
            try {
                file?.takeIf { it.exists() }?.readText()
            } catch (_: Exception) {
                null
            }
        }
    }
}
