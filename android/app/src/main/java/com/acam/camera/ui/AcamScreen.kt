package com.acam.camera.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import com.acam.camera.R
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.acam.camera.Protocol
import com.acam.camera.Session
import com.acam.camera.UiState

@Composable
fun AcamScreen(state: UiState, ips: List<String>, onToggle: () -> Unit) {
    Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Column(
            modifier = Modifier
                .safeDrawingPadding()
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(stringResource(R.string.app_name), style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.Bold)
            Text(
                stringResource(R.string.tagline),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            StatusCard(state)

            if (state.running) {
                WifiCard(ips, state.token)
                UsbCard()
            }

            state.message?.let {
                Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
            }

            Button(
                onClick = onToggle,
                modifier = Modifier.fillMaxWidth().height(56.dp),
                colors = if (state.running) {
                    ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error)
                } else {
                    ButtonDefaults.buttonColors()
                },
            ) {
                Text(stringResource(if (state.running) R.string.stop else R.string.start), fontSize = 18.sp)
            }
        }
    }
}

@Composable
private fun StatusCard(state: UiState) {
    val session = state.session
    val (icon, title, detail) = when {
        !state.running -> Triple(Icons.Filled.Info, stringResource(R.string.status_stopped), stringResource(R.string.status_stopped_detail))
        session == null -> Triple(Icons.Filled.Warning, stringResource(R.string.status_waiting), stringResource(R.string.status_waiting_detail))
        else -> Triple(Icons.Filled.CheckCircle, connectedTitle(session), sessionDetail(session))
    }
    val container = when {
        session != null -> MaterialTheme.colorScheme.primaryContainer
        state.running -> MaterialTheme.colorScheme.secondaryContainer
        else -> MaterialTheme.colorScheme.surfaceVariant
    }
    Card(colors = CardDefaults.cardColors(containerColor = container), modifier = Modifier.fillMaxWidth()) {
        Row(Modifier.padding(20.dp), verticalAlignment = Alignment.CenterVertically) {
            StatusIcon(icon)
            Spacer(Modifier.size(16.dp))
            Column {
                Text(title, style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
                Text(detail, style = MaterialTheme.typography.bodyMedium)
            }
        }
    }
}

@Composable
private fun StatusIcon(icon: ImageVector) = Icon(icon, contentDescription = null, modifier = Modifier.size(40.dp))

@Composable
private fun connectedTitle(s: Session) = stringResource(if (s.usb) R.string.connected_usb else R.string.connected_wifi)

@Composable
private fun sessionDetail(s: Session): String {
    val camera = stringResource(if (s.front) R.string.camera_front else R.string.camera_back)
    val detail = stringResource(R.string.session_detail, s.width, s.height, s.fps, camera)
    return if (s.usb) detail else stringResource(R.string.session_pc, s.peer) + "\n" + detail
}

@Composable
private fun WifiCard(ips: List<String>, token: Int) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.wifi), style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            if (ips.isEmpty()) {
                Text(stringResource(R.string.no_network), color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else {
                ips.forEach {
                    Text("$it:${Protocol.TCP_PORT}", fontFamily = FontFamily.Monospace, fontSize = 18.sp)
                }
            }
            Text(stringResource(R.string.token), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(
                token.toString(),
                fontFamily = FontFamily.Monospace,
                fontSize = 40.sp,
                fontWeight = FontWeight.Bold,
                letterSpacing = 6.sp,
            )
        }
    }
}

@Composable
private fun UsbCard() {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.usb), style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            Text(
                stringResource(R.string.usb_hint),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

@Preview(showBackground = true)
@Composable
private fun AcamScreenPreview() {
    AcamTheme {
        AcamScreen(
            UiState(running = true, token = 805569, session = Session(false, "192.168.68.129", 1280, 720, 30, false)),
            listOf("192.168.68.104"),
        ) {}
    }
}
