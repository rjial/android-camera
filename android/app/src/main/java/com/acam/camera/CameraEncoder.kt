package com.acam.camera

import android.annotation.SuppressLint
import android.content.Context
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.TotalCaptureResult
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.util.Range
import android.util.Size
import android.view.Surface
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.math.abs

/** Receives encoder output. Called on the encoder drain thread. */
interface EncoderSink {
    fun onCodecConfig(data: ByteArray)
    fun onFrame(data: ByteArray, keyframe: Boolean, ptsUs: Long)
    fun onCapabilities(caps: Map<Int, Long>)
    fun onCameraState(state: Map<Int, Long>)
}

/**
 * Camera2 → MediaCodec (Surface input) → H.264 Annex-B access units.
 * Not thread-safe; [AcamServer] calls it from one session thread.
 */
class CameraEncoder(private val context: Context, private val sink: EncoderSink) {
    private val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
    private val cameraThread = HandlerThread("acam-camera").apply { start() }
    private val cameraHandler = Handler(cameraThread.looper)

    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var codec: MediaCodec? = null
    private var inputSurface: Surface? = null
    private var drainThread: Thread? = null
    @Volatile private var running = false
    // Touched only on the camera handler thread once the session is running.
    private var controls: CameraControls? = null
    private var request: CaptureRequest.Builder? = null
    private var lastStateMs = 0L

    private val captureCallback = object : CameraCaptureSession.CaptureCallback() {
        override fun onCaptureCompleted(s: CameraCaptureSession, req: CaptureRequest, result: TotalCaptureResult) {
            val state = controls?.observe(result) ?: return
            val now = System.currentTimeMillis()
            if (now - lastStateMs >= 250) {
                lastStateMs = now
                sink.onCameraState(state)
            }
        }
    }

    var size: Size = Size(0, 0)
        private set
    var fps = 30
        private set
    var front = false
        private set
    private var bitrateKbps = 4000

    /** Opens the camera and starts encoding. Blocks until the capture session is running. */
    @Throws(IOException::class)
    fun start(width: Int, height: Int, fps: Int, bitrateKbps: Int, front: Boolean) {
        this.fps = fps.coerceIn(5, 60)
        this.bitrateKbps = bitrateKbps.coerceIn(200, 50_000)
        this.front = front
        val cameraId = findCamera(front) ?: findCamera(!front) ?: throw IOException(context.getString(R.string.err_no_camera))
        val chars = manager.getCameraCharacteristics(cameraId)
        this.front = chars.get(CameraCharacteristics.LENS_FACING) == CameraCharacteristics.LENS_FACING_FRONT
        size = chooseSize(chars, width, height)

        try {
            val codec = createCodec(size, this.fps, this.bitrateKbps)
            this.codec = codec
            inputSurface = codec.createInputSurface()
            codec.start()
            running = true
            drainThread = Thread(::drain, "acam-encoder").apply { start() }
            // Fresh camera: controls start automatic, since what is supported differs per camera.
            controls = CameraControls(chars, this.fps)
            openCamera(cameraId, chars)
        } catch (e: Exception) {
            stop()
            throw e as? IOException ?: IOException(e)
        }
        sink.onCapabilities(controls!!.capabilities())
        Log.i(TAG, "streaming camera $cameraId ${size.width}x${size.height} @ ${this.fps} fps, ${this.bitrateKbps} kbps")
    }

    fun stop() {
        running = false
        try { session?.close() } catch (_: Exception) {}
        try { device?.close() } catch (_: Exception) {}
        session = null
        device = null
        request = null
        drainThread?.join(1000)
        drainThread = null
        try { codec?.stop() } catch (_: Exception) {}
        try { codec?.release() } catch (_: Exception) {}
        codec = null
        inputSurface?.release()
        inputSurface = null
    }

    fun release() {
        stop()
        cameraThread.quitSafely()
    }

    @Throws(IOException::class)
    fun switchCamera() {
        val (w, h) = size.width to size.height
        stop()
        start(w, h, fps, bitrateKbps, !front)
    }

    fun requestKeyframe() {
        codec?.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
    }

    fun setBitrate(kbps: Int) {
        bitrateKbps = kbps.coerceIn(200, 50_000)
        codec?.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_VIDEO_BITRATE, bitrateKbps * 1000) })
    }

    /** Applies a SET_CONTROLS payload; unsupported or out-of-range values are ignored or clamped. */
    fun setControls(tlv: Map<Int, Long>) {
        cameraHandler.post {
            val c = controls ?: return@post
            val s = session ?: return@post
            val b = request ?: return@post
            val previous = c.merge(tlv)
            try {
                c.applyTo(b)
                s.setRepeatingRequest(b.build(), captureCallback, cameraHandler)
                if (c.wantsAfTrigger) {
                    b.set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_CANCEL)
                    s.capture(b.build(), captureCallback, cameraHandler)
                    b.set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_START)
                    s.capture(b.build(), captureCallback, cameraHandler)
                    b.set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_IDLE)
                }
            } catch (e: Exception) {
                // Some vendors reject particular combinations; go back to the last request that worked.
                Log.w(TAG, "camera rejected controls: $e")
                c.restore(previous)
                try {
                    c.applyTo(b)
                    s.setRepeatingRequest(b.build(), captureCallback, cameraHandler)
                } catch (_: Exception) {}
                sink.onCameraState(mapOf(Ctl.ERROR to 1L))
            }
        }
    }

    private fun findCamera(front: Boolean): String? {
        val facing = if (front) CameraCharacteristics.LENS_FACING_FRONT else CameraCharacteristics.LENS_FACING_BACK
        return manager.cameraIdList.firstOrNull {
            manager.getCameraCharacteristics(it).get(CameraCharacteristics.LENS_FACING) == facing
        }
    }

    /** Exact match if available, otherwise the supported size closest in aspect ratio and then area. */
    private fun chooseSize(chars: CameraCharacteristics, width: Int, height: Int): Size {
        val map = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
            ?: throw IOException(context.getString(R.string.err_no_stream_map))
        val caps = avcEncoderInfo().getCapabilitiesForType(MIME).videoCapabilities
        val sizes = map.getOutputSizes(MediaCodec::class.java)
            .filter { it.width % 2 == 0 && it.height % 2 == 0 && caps.isSizeSupported(it.width, it.height) }
        if (sizes.isEmpty()) throw IOException(context.getString(R.string.err_no_size))
        val wantAspect = width.toDouble() / height
        val wantArea = width.toLong() * height
        return sizes.minWith(
            compareBy<Size> { abs(it.width.toDouble() / it.height - wantAspect) > 0.01 }
                .thenBy { abs(it.width.toLong() * it.height - wantArea) },
        )
    }

    private fun avcEncoderInfo(): MediaCodecInfo =
        MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
            .filter { it.isEncoder && it.supportedTypes.any { t -> t.equals(MIME, ignoreCase = true) } }
            // Prefer hardware encoders; software ones on Q+ are flagged explicitly.
            .sortedBy { if (Build.VERSION.SDK_INT >= 29 && it.isSoftwareOnly) 1 else 0 }
            .firstOrNull() ?: throw IOException(context.getString(R.string.err_no_encoder))

    /** Tries the tuned format first, then progressively drops optional keys that some encoders reject. */
    private fun createCodec(size: Size, fps: Int, bitrateKbps: Int): MediaCodec {
        val info = avcEncoderInfo()
        var lastError: Exception? = null
        for (level in 2 downTo 0) {
            val format = buildFormat(info, size, fps, bitrateKbps, level)
            val codec = MediaCodec.createByCodecName(info.name)
            try {
                codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                Log.i(TAG, "encoder ${info.name} ${size.width}x${size.height}, option level $level")
                return codec
            } catch (e: Exception) {
                Log.w(TAG, "configure ${info.name} level $level failed: $e")
                codec.release()
                lastError = e
            }
        }
        throw IOException(context.getString(R.string.err_encoder_rejected, info.name, size.width, size.height), lastError)
    }

    /** [extras]: 2 = all tuning keys, 1 = only frame repeat, 0 = bare minimum. */
    private fun buildFormat(info: MediaCodecInfo, size: Size, fps: Int, bitrateKbps: Int, extras: Int) =
        MediaFormat.createVideoFormat(MIME, size.width, size.height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            setInteger(MediaFormat.KEY_BIT_RATE, bitrateKbps * 1000)
            setInteger(MediaFormat.KEY_FRAME_RATE, fps)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1)
            if (extras >= 1) {
                // Keep a static scene flowing so webcam consumers don't stall.
                setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, 1_000_000L / fps * 3)
            }
            if (extras >= 2) {
                setInteger(MediaFormat.KEY_PRIORITY, 0)
                if (Build.VERSION.SDK_INT >= 29) {
                    setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
                    setInteger(MediaFormat.KEY_PREPEND_HEADER_TO_SYNC_FRAMES, 1)
                }
                val baseline = info.getCapabilitiesForType(MIME).profileLevels
                    .filter { it.profile == MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline }
                    .maxByOrNull { it.level }
                if (baseline != null) {
                    setInteger(MediaFormat.KEY_PROFILE, baseline.profile)
                    setInteger(MediaFormat.KEY_LEVEL, baseline.level)
                }
            }
        }

    @SuppressLint("MissingPermission")
    private fun openCamera(cameraId: String, chars: CameraCharacteristics) {
        val surface = inputSurface!!
        val done = CountDownLatch(1)
        var error: String? = null

        manager.openCamera(cameraId, object : CameraDevice.StateCallback() {
            override fun onOpened(camera: CameraDevice) {
                device = camera
                @Suppress("DEPRECATION")
                camera.createCaptureSession(listOf(surface), object : CameraCaptureSession.StateCallback() {
                    override fun onConfigured(s: CameraCaptureSession) {
                        session = s
                        val builder = camera.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply {
                            addTarget(surface)
                            chooseFpsRange(chars)?.let { set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, it) }
                        }
                        request = builder
                        try {
                            controls?.applyTo(builder)
                            s.setRepeatingRequest(builder.build(), captureCallback, cameraHandler)
                        } catch (e: Exception) {
                            error = "setRepeatingRequest: $e"
                        }
                        done.countDown()
                    }

                    override fun onConfigureFailed(s: CameraCaptureSession) {
                        error = context.getString(R.string.err_session_failed)
                        done.countDown()
                    }
                }, cameraHandler)
            }

            override fun onDisconnected(camera: CameraDevice) {
                camera.close()
                error = error ?: context.getString(R.string.err_camera_disconnected)
                done.countDown()
            }

            override fun onError(camera: CameraDevice, code: Int) {
                camera.close()
                error = context.getString(R.string.err_camera_error, code)
                done.countDown()
            }
        }, cameraHandler)

        if (!done.await(5, TimeUnit.SECONDS)) throw IOException(context.getString(R.string.err_camera_timeout))
        error?.let { throw IOException(it) }
    }

    /** Fixed range at the target fps if offered, else the range containing it with the narrowest span. */
    private fun chooseFpsRange(chars: CameraCharacteristics): Range<Int>? {
        val ranges = chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES) ?: return null
        return ranges.filter { it.contains(fps) }
            .minByOrNull { (it.upper - it.lower) * 1000 - it.lower }
            ?: ranges.maxByOrNull { it.upper }
    }

    private fun drain() {
        val info = MediaCodec.BufferInfo()
        while (running) {
            val codec = codec ?: break
            val index = try {
                codec.dequeueOutputBuffer(info, 100_000)
            } catch (e: IllegalStateException) {
                break
            }
            if (index < 0) continue
            val buffer = codec.getOutputBuffer(index)
            if (buffer != null && info.size > 0) {
                val data = ByteArray(info.size)
                buffer.position(info.offset)
                buffer.get(data)
                val idr = hasIdr(data)
                // Some encoders (e.g. Qualcomm) flag SPS+PPS+IDR as codec config; that buffer is really a keyframe.
                if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0 && !idr) {
                    sink.onCodecConfig(data)
                } else {
                    val key = idr || info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
                    sink.onFrame(data, key, info.presentationTimeUs)
                }
            }
            codec.releaseOutputBuffer(index, false)
        }
    }

    /** True if the Annex-B buffer contains an IDR slice (NAL type 5). */
    private fun hasIdr(data: ByteArray): Boolean {
        for (i in 0 until data.size - 3) {
            if (data[i] == 0.toByte() && data[i + 1] == 0.toByte() && data[i + 2] == 1.toByte() &&
                data[i + 3].toInt() and 0x1f == 5
            ) return true
        }
        return false
    }

    companion object {
        private const val TAG = "CameraEncoder"
        private const val MIME = MediaFormat.MIMETYPE_VIDEO_AVC
    }
}
