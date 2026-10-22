package com.acam.camera

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import java.security.SecureRandom

/** Foreground service that owns the TCP server and discovery so streaming survives the screen turning off. */
class StreamService : Service() {
    private var server: AcamServer? = null
    private var discovery: Discovery? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var usbLink: UsbLink? = null

    private val usbReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            @Suppress("DEPRECATION")
            val acc = intent.getParcelableExtra<UsbAccessory>(UsbManager.EXTRA_ACCESSORY)
            when (intent.action) {
                UsbManager.ACTION_USB_ACCESSORY_DETACHED -> if (acc == null || UsbLink.isAcam(acc)) {
                    usbLink?.stop()
                    StreamState.update { it.copy(message = getString(R.string.usb_unplugged)) }
                }
                ACTION_USB_PERMISSION -> if (acc != null && intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)) {
                    usbLink?.start(acc)
                }
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (server != null) {
            checkUsb()
            return START_NOT_STICKY
        }

        val notification = buildNotification()
        if (Build.VERSION.SDK_INT >= 30) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }

        val t = SecureRandom().nextInt(900_000) + 100_000
        try {
            server = AcamServer(
                applicationContext,
                Protocol.TCP_PORT,
                t,
                onSession = { s -> StreamState.update { it.copy(session = s, message = null) } },
                onMessage = { m -> StreamState.update { it.copy(message = m) } },
            ).also { it.start() }
        } catch (e: Exception) {
            Log.e(TAG, "server failed to start", e)
            StreamState.update { it.copy(message = getString(R.string.port_failed, Protocol.TCP_PORT, e.message ?: "")) }
            stopSelf()
            return START_NOT_STICKY
        }
        discovery = Discovery(Protocol.TCP_PORT).also { it.start() }
        wakeLock = (getSystemService(Context.POWER_SERVICE) as PowerManager)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "acam:stream")
            .apply { acquire() }
        StreamState.update { UiState(running = true, token = t) }

        usbLink = UsbLink(applicationContext, server!!)
        val filter = IntentFilter().apply {
            addAction(UsbManager.ACTION_USB_ACCESSORY_DETACHED)
            addAction(ACTION_USB_PERMISSION)
        }
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(usbReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            registerReceiver(usbReceiver, filter)
        }
        checkUsb()
        return START_NOT_STICKY
    }

    /** Starts the USB link if the ACAM PC client is attached, asking for permission when needed. */
    private fun checkUsb() {
        val usb = getSystemService(Context.USB_SERVICE) as UsbManager
        val acc = usb.accessoryList?.firstOrNull(UsbLink::isAcam) ?: return
        if (usb.hasPermission(acc)) {
            usbLink?.start(acc)
        } else {
            val flags = if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0
            val pi = PendingIntent.getBroadcast(this, 0, Intent(ACTION_USB_PERMISSION).setPackage(packageName), flags)
            usb.requestPermission(acc, pi)
        }
    }

    override fun onDestroy() {
        if (usbLink != null) unregisterReceiver(usbReceiver)
        usbLink?.stop()
        usbLink = null
        discovery?.stop()
        server?.stop()
        wakeLock?.release()
        server = null
        StreamState.update { UiState(message = it.message) }
        super.onDestroy()
    }

    private fun buildNotification(): Notification {
        val nm = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= 26) {
            nm.createNotificationChannel(NotificationChannel(CHANNEL, getString(R.string.notification_channel), NotificationManager.IMPORTANCE_LOW))
        }
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        @Suppress("DEPRECATION")
        val builder = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL) else Notification.Builder(this)
        return builder
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(getString(R.string.notification_text, Protocol.TCP_PORT))
            .setSmallIcon(android.R.drawable.ic_menu_camera)
            .setContentIntent(open)
            .setOngoing(true)
            .build()
    }

    companion object {
        private const val TAG = "StreamService"
        private const val CHANNEL = "acam"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_USB_PERMISSION = "com.acam.camera.USB_PERMISSION"
    }
}
