package com.acam.camera

import android.content.Context
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.util.Log
import java.io.FileInputStream
import java.io.FileOutputStream
import java.util.concurrent.Semaphore

/**
 * Keeps an ACAM session listening on a USB accessory (the PC running `acam --usb`).
 * After each session ends the accessory is reopened so the PC can reconnect without replugging.
 */
class UsbLink(private val context: Context, private val server: AcamServer) {
    private val usb = context.getSystemService(Context.USB_SERVICE) as UsbManager
    @Volatile private var accessory: UsbAccessory? = null
    private var thread: Thread? = null

    val connected get() = accessory != null

    @Synchronized
    fun start(acc: UsbAccessory) {
        // Only one ACAM accessory can exist; restarting would kill a live session (e.g. when the
        // "open with ACAM" dialog is confirmed while already streaming).
        if (thread?.isAlive == true) return
        stop()
        accessory = acc
        thread = Thread({ loop(acc) }, "acam-usb").apply { start() }
    }

    @Synchronized
    fun stop() {
        accessory = null
        thread?.interrupt()
        thread = null
        server.detach(PEER)
    }

    private fun loop(acc: UsbAccessory) {
        val ended = Semaphore(0)
        while (accessory == acc && !Thread.currentThread().isInterrupted) {
            // Fails with EBUSY while a previous session's blocked read still holds the device; just retry.
            val pfd = try { usb.openAccessory(acc) } catch (e: Exception) { null }
            if (pfd == null) {
                if (!sleep(500)) return
                continue
            }
            Log.i(TAG, "accessory opened, waiting for the PC handshake")
            server.attach(
                FileInputStream(pfd.fileDescriptor),
                FileOutputStream(pfd.fileDescriptor),
                PEER,
                requireToken = false,
                closer = { pfd.close() },
                onEnd = { ended.release() },
            )
            try {
                ended.acquire()
            } catch (_: InterruptedException) {
                return
            }
            if (!sleep(300)) return
        }
    }

    private fun sleep(ms: Long) = try {
        Thread.sleep(ms)
        true
    } catch (_: InterruptedException) {
        false
    }

    companion object {
        private const val TAG = "UsbLink"
        const val PEER = "USB"

        /** True if [acc] is the PC-side ACAM client (see accessory_filter.xml and pc/src/usb.rs). */
        fun isAcam(acc: UsbAccessory) = acc.manufacturer == "ACAM" && acc.model == "ACAM Webcam"
    }
}
