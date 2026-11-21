package io.github.teamclouday.androidMic.domain.audio

import org.junit.Assert.assertEquals
import org.junit.Test

class LockPolicyOverrideParserTest {

    @Test
    fun `missing file body resolves to immediate`() {
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideParser.parse(null))
    }

    @Test
    fun `empty or blank file resolves to immediate`() {
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideParser.parse(""))
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideParser.parse("   \n  "))
    }

    @Test
    fun `unrecognized value resolves to immediate`() {
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideParser.parse("foo"))
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideParser.parse("grace"))
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideParser.parse("always30"))
    }

    @Test
    fun `utf-8 bom prefix makes the value unrecognized`() {
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideParser.parse("\uFEFFalways"))
    }

    @Test
    fun `valid values parse case-insensitively with trimming`() {
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideParser.parse("immediate"))
        assertEquals(CaptureLockPolicy.GRACE_30S, LockPolicyOverrideParser.parse("grace30s"))
        assertEquals(CaptureLockPolicy.ALWAYS, LockPolicyOverrideParser.parse("always"))
        assertEquals(CaptureLockPolicy.GRACE_30S, LockPolicyOverrideParser.parse("  Grace30S \n"))
        assertEquals(CaptureLockPolicy.ALWAYS, LockPolicyOverrideParser.parse("  ALWAYS"))
    }

    @Test
    fun `source delegates to the parser through the injected reader`() {
        val immediate = LockPolicyOverrideSource { null }
        assertEquals(CaptureLockPolicy.IMMEDIATE, immediate.read())

        val always = LockPolicyOverrideSource { "always" }
        assertEquals(CaptureLockPolicy.ALWAYS, always.read())

        val invalid = LockPolicyOverrideSource { "nope" }
        assertEquals(CaptureLockPolicy.IMMEDIATE, invalid.read())
    }
}
