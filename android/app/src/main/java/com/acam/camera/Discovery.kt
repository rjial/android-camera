package com.acam.camera

import android.os.Build
import java.io.ByteArrayOutputStream
import java.io.DataOutputStream
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.Inet4Address
import java.net.InetAddress
import java.net.NetworkInterface

/** Broadcasts an ACAM announce once per second so the PC can find this phone with `--discover`. */
class Discovery(private val tcpPort: Int) {
    @Volatile private var running = false
    private var thread: Thread? = null

    fun start() {
        running = true
        thread = Thread({
            val payload = announce()
            DatagramSocket().use { socket ->
                socket.broadcast = true
                while (running) {
                    for (target in broadcastAddresses()) {
                        try {
                            socket.send(DatagramPacket(payload, payload.size, target, Protocol.DISCOVERY_PORT))
                        } catch (_: Exception) {}
                    }
                    try { Thread.sleep(1000) } catch (_: InterruptedException) { break }
                }
            }
        }, "acam-discovery").apply { start() }
    }

    fun stop() {
        running = false
        thread?.interrupt()
    }

    private fun announce(): ByteArray {
        val name = Build.MODEL.toByteArray().take(255).toByteArray()
        val bytes = ByteArrayOutputStream()
        DataOutputStream(bytes).apply {
            write(Protocol.MAGIC)
            writeByte(Protocol.VERSION)
            writeShort(tcpPort)
            writeByte(name.size)
            write(name)
        }
        return bytes.toByteArray()
    }

    private fun broadcastAddresses(): List<InetAddress> =
        NetworkInterface.getNetworkInterfaces().toList()
            .filter { it.isUp && !it.isLoopback }
            .flatMap { it.interfaceAddresses }
            .mapNotNull { it.broadcast }
            .ifEmpty { listOf(InetAddress.getByName("255.255.255.255")) }

    companion object {
        /** IPv4 addresses of active non-loopback interfaces, Wi-Fi first. */
        fun localAddresses(): List<String> =
            NetworkInterface.getNetworkInterfaces().toList()
                .filter { it.isUp && !it.isLoopback }
                .sortedBy { if (it.name.startsWith("wlan")) 0 else 1 }
                .flatMap { it.inetAddresses.toList() }
                .filterIsInstance<Inet4Address>()
                .mapNotNull { it.hostAddress }
    }
}
