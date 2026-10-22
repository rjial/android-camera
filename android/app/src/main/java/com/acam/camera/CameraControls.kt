package com.acam.camera

import android.graphics.Rect
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraMetadata
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.CaptureResult
import android.hardware.camera2.params.ColorSpaceTransform
import android.hardware.camera2.params.RggbChannelVector
import android.os.Build
import android.util.Range
import kotlin.math.ln
import kotlin.math.pow
import kotlin.math.roundToInt
import kotlin.math.roundToLong

/** Requested manual settings. Defaults are "everything automatic". */
data class ControlState(
    val aeManual: Boolean = false,
    val iso: Int = 0,
    val exposureNs: Long = 0,
    val evSteps: Int = 0,
    val aeLock: Boolean = false,
    /** 0 continuous, 1 manual distance, 2 focus once then hold. */
    val afMode: Int = AF_CONTINUOUS,
    val focusDiopters: Float = 0f,
    /** CONTROL_AWB_MODE_*; [CaptureRequest.CONTROL_AWB_MODE_OFF] means manual Kelvin. */
    val awbMode: Int = CaptureRequest.CONTROL_AWB_MODE_AUTO,
    val kelvin: Int = 5500,
    val awbLock: Boolean = false,
    val zoom: Float = 1f,
) {
    companion object {
        const val AF_CONTINUOUS = 0
        const val AF_MANUAL = 1
        const val AF_ONCE = 2
    }
}

/**
 * What one camera supports, read from its characteristics, and how a [ControlState] maps onto a
 * capture request. Every feature is optional: devices and individual cameras differ widely, so
 * anything not explicitly advertised is treated as unavailable and silently ignored.
 */
class CameraControls(private val chars: CameraCharacteristics, private val fps: Int) {
    private val capabilities = chars.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES)?.toSet() ?: emptySet()
    private val requestKeys = chars.availableCaptureRequestKeys.toSet()
    private val aeModes = chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_MODES)?.toSet() ?: emptySet()
    private val afModes = chars.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES)?.toSet() ?: emptySet()
    private val awbModes = chars.get(CameraCharacteristics.CONTROL_AWB_AVAILABLE_MODES)?.toSet() ?: emptySet()
    private val isoRange: Range<Int>? = chars.get(CameraCharacteristics.SENSOR_INFO_SENSITIVITY_RANGE)
    private val exposureRange: Range<Long>? = chars.get(CameraCharacteristics.SENSOR_INFO_EXPOSURE_TIME_RANGE)
    private val evRange: Range<Int>? = chars.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)
    private val evStep = chars.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_STEP)
    private val minFocus = chars.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) ?: 0f
    private val activeArray: Rect? = chars.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE)
    private val zoomRatioRange: Range<Float>? =
        if (Build.VERSION.SDK_INT >= 30) chars.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE) else null
    private val maxDigitalZoom = chars.get(CameraCharacteristics.SCALER_AVAILABLE_MAX_DIGITAL_ZOOM) ?: 1f

    private val frameDurationNs = 1_000_000_000L / fps

    val manualSensor = CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_SENSOR in capabilities &&
        CameraMetadata.CONTROL_AE_MODE_OFF in aeModes && isoRange != null && exposureRange != null
    val manualFocus = (CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_SENSOR in capabilities ||
        CaptureRequest.LENS_FOCUS_DISTANCE in requestKeys) &&
        CameraMetadata.CONTROL_AF_MODE_OFF in afModes && minFocus > 0f
    private val afContinuous = CameraMetadata.CONTROL_AF_MODE_CONTINUOUS_VIDEO in afModes
    private val afAuto = CameraMetadata.CONTROL_AF_MODE_AUTO in afModes
    val manualWb = CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_POST_PROCESSING in capabilities &&
        CameraMetadata.CONTROL_AWB_MODE_OFF in awbModes
    private val evSupported = evRange != null && (evRange.lower != 0 || evRange.upper != 0)
    private val aeLockAvailable = chars.get(CameraCharacteristics.CONTROL_AE_LOCK_AVAILABLE) == true
    private val awbLockAvailable = chars.get(CameraCharacteristics.CONTROL_AWB_LOCK_AVAILABLE) == true
    private val zoomMax = zoomRatioRange?.upper ?: maxDigitalZoom
    private val zoomMin = zoomRatioRange?.lower ?: 1f

    /** Latest automatic color transform/gains, the base for manual Kelvin white balance. */
    @Volatile private var autoTransform: ColorSpaceTransform? = null
    @Volatile private var autoGains: RggbChannelVector? = null
    private var kelvinBase: RggbChannelVector? = null

    var state = ControlState()
        private set

    /** CAPABILITIES payload (see PROTOCOL.md). Absent keys mean "not supported". */
    fun capabilities(): Map<Int, Long> = buildMap {
        put(Cap.MANUAL_SENSOR, if (manualSensor) 1 else 0)
        if (manualSensor) {
            put(Cap.ISO_MIN, isoRange!!.lower.toLong())
            put(Cap.ISO_MAX, isoRange.upper.toLong())
            put(Cap.EXPOSURE_MIN_NS, exposureRange!!.lower)
            put(Cap.EXPOSURE_MAX_NS, exposureRange.upper)
        }
        if (evSupported) {
            put(Cap.EV_MIN, evRange!!.lower.toLong())
            put(Cap.EV_MAX, evRange.upper.toLong())
            put(Cap.EV_STEP_MILLI, ((evStep?.toFloat() ?: 0f) * 1000).roundToLong())
        }
        put(Cap.MIN_FOCUS_MILLI_DIOPTER, if (manualFocus) (minFocus * 1000).roundToLong() else 0)
        put(Cap.AUTOFOCUS, if (afContinuous || afAuto) 1 else 0)
        put(Cap.AWB_MODES, awbModes.filter { it in 1..62 }.fold(0L) { m, mode -> m or (1L shl mode) })
        put(Cap.MANUAL_WB, if (manualWb) 1 else 0)
        put(Cap.ZOOM_MIN_MILLI, (zoomMin * 1000).roundToLong())
        put(Cap.ZOOM_MAX_MILLI, (zoomMax * 1000).roundToLong())
        put(Cap.AE_LOCK, if (aeLockAvailable) 1 else 0)
        put(Cap.AWB_LOCK, if (awbLockAvailable) 1 else 0)
        put(Cap.FRAME_DURATION_NS, frameDurationNs)
    }

    /** Merges a SET_CONTROLS payload. Returns the previous state so a rejected request can be rolled back. */
    fun merge(tlv: Map<Int, Long>): ControlState {
        val previous = state
        var s = state
        tlv.forEach { (key, v) ->
            s = when (key) {
                Ctl.AE_MODE -> s.copy(aeManual = v == 1L && manualSensor)
                Ctl.ISO -> s.copy(iso = v.toInt())
                Ctl.EXPOSURE_NS -> s.copy(exposureNs = v)
                Ctl.EV_STEPS -> s.copy(evSteps = v.toInt())
                Ctl.AE_LOCK -> s.copy(aeLock = v != 0L)
                Ctl.AF_MODE -> s.copy(afMode = v.toInt())
                Ctl.FOCUS_MILLI_DIOPTER -> s.copy(focusDiopters = v / 1000f)
                Ctl.AF_TRIGGER -> if (afAuto || afContinuous) s.copy(afMode = ControlState.AF_ONCE) else s
                Ctl.AWB_MODE -> s.copy(awbMode = v.toInt())
                Ctl.WB_KELVIN -> s.copy(kelvin = v.toInt().coerceIn(1500, 15000))
                Ctl.AWB_LOCK -> s.copy(awbLock = v != 0L)
                Ctl.ZOOM_MILLI -> s.copy(zoom = v / 1000f)
                else -> s
            }
        }
        // Entering manual Kelvin: anchor on the gains auto white balance was using just before.
        if (s.awbMode == CaptureRequest.CONTROL_AWB_MODE_OFF && previous.awbMode != CaptureRequest.CONTROL_AWB_MODE_OFF) {
            kelvinBase = autoGains
        }
        state = s
        return previous
    }

    fun restore(previous: ControlState) {
        state = previous
    }

    fun applyTo(b: CaptureRequest.Builder) {
        val s = state
        b.set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)

        if (s.aeManual && manualSensor) {
            b.set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
            val iso = if (s.iso > 0) s.iso else isoRange!!.lower * 4
            b.set(CaptureRequest.SENSOR_SENSITIVITY, iso.coerceIn(isoRange!!.lower, isoRange.upper))
            val exp = (if (s.exposureNs > 0) s.exposureNs else frameDurationNs / 2)
                .coerceIn(exposureRange!!.lower, minOf(exposureRange.upper, MAX_EXPOSURE_NS))
            b.set(CaptureRequest.SENSOR_EXPOSURE_TIME, exp)
            // An exposure longer than one frame stretches the frame, so the stream fps drops to match.
            b.set(CaptureRequest.SENSOR_FRAME_DURATION, maxOf(frameDurationNs, exp))
        } else {
            b.set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON)
            if (evSupported) {
                b.set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION, s.evSteps.coerceIn(evRange!!.lower, evRange.upper))
            }
            if (aeLockAvailable) b.set(CaptureRequest.CONTROL_AE_LOCK, s.aeLock)
        }

        when {
            s.afMode == ControlState.AF_MANUAL && manualFocus -> {
                b.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_OFF)
                b.set(CaptureRequest.LENS_FOCUS_DISTANCE, s.focusDiopters.coerceIn(0f, minFocus))
            }
            s.afMode == ControlState.AF_ONCE && afAuto ->
                b.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_AUTO)
            afContinuous -> b.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO)
            afAuto -> b.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_AUTO)
        }

        if (s.awbMode == CaptureRequest.CONTROL_AWB_MODE_OFF && manualWb) {
            b.set(CaptureRequest.CONTROL_AWB_MODE, CaptureRequest.CONTROL_AWB_MODE_OFF)
            b.set(CaptureRequest.COLOR_CORRECTION_MODE, CaptureRequest.COLOR_CORRECTION_MODE_TRANSFORM_MATRIX)
            b.set(CaptureRequest.COLOR_CORRECTION_GAINS, kelvinGains(s.kelvin))
            b.set(CaptureRequest.COLOR_CORRECTION_TRANSFORM, autoTransform ?: IDENTITY)
        } else {
            val mode = if (s.awbMode in awbModes && s.awbMode != CaptureRequest.CONTROL_AWB_MODE_OFF) {
                s.awbMode
            } else {
                CaptureRequest.CONTROL_AWB_MODE_AUTO
            }
            b.set(CaptureRequest.CONTROL_AWB_MODE, mode)
            if (manualWb) b.set(CaptureRequest.COLOR_CORRECTION_MODE, CaptureRequest.COLOR_CORRECTION_MODE_FAST)
            if (awbLockAvailable) b.set(CaptureRequest.CONTROL_AWB_LOCK, s.awbLock)
        }

        val zoom = s.zoom.coerceIn(zoomMin, zoomMax)
        if (Build.VERSION.SDK_INT >= 30 && zoomRatioRange != null) {
            b.set(CaptureRequest.CONTROL_ZOOM_RATIO, zoom)
        } else if (maxDigitalZoom > 1f && activeArray != null) {
            val w = (activeArray.width() / zoom).roundToInt()
            val h = (activeArray.height() / zoom).roundToInt()
            val l = activeArray.left + (activeArray.width() - w) / 2
            val t = activeArray.top + (activeArray.height() - h) / 2
            b.set(CaptureRequest.SCALER_CROP_REGION, Rect(l, t, l + w, t + h))
        }
    }

    val wantsAfTrigger get() = state.afMode == ControlState.AF_ONCE

    /** Records automatic color values and returns the CAMERA_STATE payload for [r]. */
    fun observe(r: CaptureResult): Map<Int, Long> {
        if (r.get(CaptureResult.CONTROL_AWB_MODE) != CaptureResult.CONTROL_AWB_MODE_OFF) {
            r.get(CaptureResult.COLOR_CORRECTION_TRANSFORM)?.let { autoTransform = it }
            r.get(CaptureResult.COLOR_CORRECTION_GAINS)?.let { autoGains = it }
        }
        return buildMap {
            r.get(CaptureResult.SENSOR_SENSITIVITY)?.let { put(Ctl.ISO, it.toLong()) }
            r.get(CaptureResult.SENSOR_EXPOSURE_TIME)?.let { put(Ctl.EXPOSURE_NS, it) }
            r.get(CaptureResult.LENS_FOCUS_DISTANCE)?.let { put(Ctl.FOCUS_MILLI_DIOPTER, (it * 1000).roundToLong()) }
            val zoom = if (Build.VERSION.SDK_INT >= 30) r.get(CaptureResult.CONTROL_ZOOM_RATIO) else null
            put(Ctl.ZOOM_MILLI, ((zoom ?: state.zoom.coerceIn(zoomMin, zoomMax)) * 1000).roundToLong())
        }
    }

    /**
     * White balance gains for an illuminant of [kelvin], relative to what auto WB chose (assumed ~5500 K).
     * Sensor-space gains can't be derived from Kelvin alone, so this is an approximation.
     */
    private fun kelvinGains(kelvin: Int): RggbChannelVector {
        val base = kelvinBase ?: autoGains ?: RggbChannelVector(2f, 1f, 1f, 1.6f)
        val ref = blackbody(5500)
        val ill = blackbody(kelvin)
        val r = base.red * (ref[0] / ill[0]) / (ref[1] / ill[1])
        val b = base.blue * (ref[2] / ill[2]) / (ref[1] / ill[1])
        val g = (base.greenEven + base.greenOdd) / 2
        return RggbChannelVector(r.coerceIn(0.25f, 8f), g, g, b.coerceIn(0.25f, 8f))
    }

    companion object {
        /** Longer shutters would make the stream unusably slow. */
        private const val MAX_EXPOSURE_NS = 1_000_000_000L
        private val IDENTITY = ColorSpaceTransform(intArrayOf(1, 1, 0, 1, 0, 1, 0, 1, 1, 1, 0, 1, 0, 1, 0, 1, 1, 1))

        /** Approximate RGB (0..1) of a black body at [kelvin] (Tanner Helland's fit). */
        fun blackbody(kelvin: Int): FloatArray {
            val t = kelvin / 100.0
            val r = if (t <= 66) 255.0 else 329.698727446 * (t - 60).pow(-0.1332047592)
            val g = if (t <= 66) 99.4708025861 * ln(t) - 161.1195681661 else 288.1221695283 * (t - 60).pow(-0.0755148492)
            val b = when {
                t >= 66 -> 255.0
                t <= 19 -> 0.0
                else -> 138.5177312231 * ln(t - 10) - 305.0447927307
            }
            return doubleArrayOf(r, g, b).map { (it.coerceIn(1.0, 255.0) / 255.0).toFloat() }.toFloatArray()
        }
    }
}
