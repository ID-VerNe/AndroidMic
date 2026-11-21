package io.github.teamclouday.androidMic.domain.audio

import org.junit.Assert.assertEquals
import org.junit.Test
import java.io.File

class LockPolicyOverrideSourceTest {

    @Test
    fun `missing file falls back to immediate`() {
        val missing = File(System.getProperty("java.io.tmpdir")!!, "lpo-missing-${System.nanoTime()}.txt")
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideSource.fromFile(missing).read())
    }

    @Test
    fun `unreadable file (directory) falls back to immediate`() {
        val dir = File(System.getProperty("java.io.tmpdir")!!)
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideSource.fromFile(dir).read())
    }

    @Test
    fun `valid temp file value takes effect`() {
        val file = File.createTempFile("lpo", ".txt")
        file.deleteOnExit()
        file.writeText("always")
        assertEquals(CaptureLockPolicy.ALWAYS, LockPolicyOverrideSource.fromFile(file).read())
    }

    @Test
    fun `empty temp file falls back to immediate`() {
        val file = File.createTempFile("lpo", ".txt")
        file.deleteOnExit()
        file.writeText("")
        assertEquals(CaptureLockPolicy.IMMEDIATE, LockPolicyOverrideSource.fromFile(file).read())
    }
}
