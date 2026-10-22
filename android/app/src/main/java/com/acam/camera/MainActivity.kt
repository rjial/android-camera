package com.acam.camera

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.acam.camera.ui.AcamScreen
import com.acam.camera.ui.AcamTheme
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

class MainActivity : ComponentActivity() {
    private val permissions = registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            startStreaming()
        } else {
            StreamState.update { it.copy(message = getString(R.string.camera_permission_needed)) }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        setContent {
            AcamTheme {
                val state by StreamState.state.collectAsStateWithLifecycle()
                var ips by remember { mutableStateOf(emptyList<String>()) }
                // Addresses change when Wi-Fi reconnects; cheap enough to poll.
                LaunchedEffect(Unit) {
                    while (true) {
                        ips = withContext(Dispatchers.IO) { Discovery.localAddresses() }
                        delay(2000)
                    }
                }
                AcamScreen(state, ips, onToggle = ::onToggle)
            }
        }
    }

    override fun onStart() {
        super.onStart()
        handleUsbIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleUsbIntent(intent)
    }

    /** Launched because the PC client attached over USB: start streaming without needing the button. */
    private fun handleUsbIntent(intent: Intent?) {
        if (intent?.action != UsbManager.ACTION_USB_ACCESSORY_ATTACHED) return
        setIntent(Intent(this, MainActivity::class.java))
        if (StreamState.running) {
            startStreaming() // service re-checks the accessory list
        } else {
            onToggle()
        }
    }

    private fun onToggle() {
        if (StreamState.running) {
            stopService(Intent(this, StreamService::class.java))
            return
        }
        val missing = buildList {
            if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
                add(Manifest.permission.CAMERA)
            }
            if (Build.VERSION.SDK_INT >= 33 &&
                checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
            ) {
                add(Manifest.permission.POST_NOTIFICATIONS)
            }
        }
        if (missing.isNotEmpty()) permissions.launch(missing.toTypedArray()) else startStreaming()
    }

    private fun startStreaming() {
        StreamState.update { it.copy(message = null) }
        val intent = Intent(this, StreamService::class.java)
        if (Build.VERSION.SDK_INT >= 26) startForegroundService(intent) else startService(intent)
    }
}
