package com.acam.camera

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/** The PC currently streaming from this phone. */
data class Session(
    val usb: Boolean,
    val peer: String,
    val width: Int,
    val height: Int,
    val fps: Int,
    val front: Boolean,
)

data class UiState(
    val running: Boolean = false,
    val token: Int = 0,
    val session: Session? = null,
    /** Last notable event (rejected client, port error, cable unplugged, ...). */
    val message: String? = null,
)

/** Single source of truth shared by [StreamService] (writer) and the Compose UI (reader). */
object StreamState {
    private val _state = MutableStateFlow(UiState())
    val state: StateFlow<UiState> = _state.asStateFlow()

    val running get() = _state.value.running

    fun update(f: (UiState) -> UiState) = _state.update(f)
}
