package io.github.teamclouday.androidMic.domain.streaming

/**
 * Connect policy of the `AUTO` transport, kept pure so it can be unit tested.
 *
 * The PC listens on one wildcard socket, so from the phone's point of view the
 * two paths differ only in the address it dials: the `adb reverse` tunnel on
 * loopback when a cable is in, the configured LAN address otherwise. Trying
 * loopback first is what makes "cable plugged in" win without the phone being
 * able to observe the cable itself.
 */
object AutoTransport {

    const val LOOPBACK = "127.0.0.1"

    /** per-candidate TCP connect timeout, same as the manual transports */
    const val CONNECT_TIMEOUT_MS = 1500

    /** how long a dropped Auto stream keeps retrying before it gives up */
    const val RECONNECT_WINDOW_MS = 8_000L

    /** pause between reconnect rounds (each round walks every candidate) */
    const val RECONNECT_DELAY_MS = 500L

    private val IPV4 = Regex("^(\\d{1,3})\\.(\\d{1,3})\\.(\\d{1,3})\\.(\\d{1,3})$")

    /**
     * The stored IP preference defaults to the `192.168.` prefix, which
     * `checkIp` accepts but no socket can connect to. Only a complete IPv4
     * literal that is not loopback is a usable fallback address.
     */
    fun usableLanIp(raw: String?): String? {
        val value = raw?.trim()?.takeIf { it.isNotEmpty() } ?: return null
        val match = IPV4.matchEntire(value) ?: return null
        if (match.groupValues.drop(1).any { it.toInt() > 255 }) return null
        if (value == LOOPBACK) return null
        return value
    }

    /**
     * Addresses to try, in order. A blank or loopback LAN address collapses to
     * the loopback entry only, which is the documented "USB only" state.
     */
    fun candidates(lanIp: String?): List<String> {
        return listOfNotNull(LOOPBACK, usableLanIp(lanIp)).distinct()
    }
}
