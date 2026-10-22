package com.acam.camera

import android.content.Context
import android.util.Log
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * Serves ACAM clients over TCP or USB accessory. Any number of connections may be handshaking, but only
 * one session owns the camera: a client that completes a valid handshake takes over from the previous one.
 */
class AcamServer(
    private val context: Context,
    private val port: Int,
    private val token: Int,
    private val onSession: (Session?) -> Unit,
    private val onMessage: (String) -> Unit,
) {
    private var server: ServerSocket? = null
    private var acceptThread: Thread? = null
    @Volatile private var current: ClientSession? = null
    private val sessions = mutableSetOf<ClientSession>()

    fun start() {
        val s = ServerSocket().apply {
            reuseAddress = true
            bind(InetSocketAddress(port))
        }
        server = s
        acceptThread = Thread({
            while (!s.isClosed) {
                val socket = try { s.accept() } catch (_: IOException) { break }
                try {
                    socket.tcpNoDelay = true
                    socket.soTimeout = 6000
                    socket.sendBufferSize = 512 * 1024
                    attach(socket.getInputStream(), socket.getOutputStream(), socket.inetAddress.hostAddress ?: "?",
                        requireToken = true, closer = { socket.close() })
                } catch (e: IOException) {
                    socket.close()
                }
            }
        }, "acam-accept").apply { start() }
    }

    /**
     * Starts a session on an already-open byte stream. [requireToken] is false for USB, where
     * plugging in the cable is the pairing step.
     */
    fun attach(
        input: InputStream,
        output: OutputStream,
        peer: String,
        requireToken: Boolean,
        closer: () -> Unit,
        onEnd: () -> Unit = {},
    ) {
        val session = ClientSession(input, output, peer, requireToken, closer, onEnd)
        synchronized(sessions) { sessions += session }
        Thread(session::run, "acam-session").start()
    }

    /** Closes every session (handshaking or active) whose peer is [peer]. */
    fun detach(peer: String) {
        synchronized(sessions) { sessions.filter { it.peer == peer } }.forEach { it.close() }
    }

    fun stop() {
        try { server?.close() } catch (_: IOException) {}
        synchronized(sessions) { sessions.toList() }.forEach { it.close() }
        acceptThread?.join(1000)
    }

    private fun takeOver(session: ClientSession) {
        val previous = synchronized(sessions) {
            current.also { current = session }
        }
        if (previous !== session) previous?.close()
    }

    private inner class ClientSession(
        private val rawInput: InputStream,
        private val rawOutput: OutputStream,
        val peer: String,
        private val requireToken: Boolean,
        private val closer: () -> Unit,
        private val onEnd: () -> Unit,
    ) : EncoderSink {
        private val queue = ArrayBlockingQueue<Packet>(QUEUE_FRAMES)
        private lateinit var out: DataOutputStream
        private var encoder: CameraEncoder? = null
        @Volatile private var closed = false
        // Set when the queue overflowed; P-frames are skipped until the next keyframe.
        @Volatile private var dropUntilKeyframe = false
        @Volatile private var codecConfig: ByteArray? = null
        // USB accessory streams have no read timeout, so idle sessions are detected here instead.
        @Volatile private var lastRxMs = System.currentTimeMillis()
        @Volatile private var configSent = false
        @Volatile private var pendingCaps: Map<Int, Long>? = null

        fun run() {
            try {
                // USB accessory reads must be at least one full USB packet, so read in large chunks.
                val input = DataInputStream(BufferedInputStream(rawInput, 16 * 1024))
                out = DataOutputStream(BufferedOutputStream(rawOutput, 64 * 1024))

                val hello = Hello.read(input)
                if (hello.version != Protocol.VERSION) return reject(Protocol.STATUS_BAD_VERSION, context.getString(R.string.reject_version, hello.version))
                if (requireToken && hello.token != token) return reject(Protocol.STATUS_BAD_TOKEN, context.getString(R.string.reject_token))
                takeOver(this)
                // The idle watchdog counts from the handshake, not from when the stream was opened.
                lastRxMs = System.currentTimeMillis()

                val enc = CameraEncoder(context, this)
                encoder = enc
                try {
                    enc.start(hello.width, hello.height, hello.fps, hello.bitrateKbps, hello.camera == 1)
                } catch (e: Exception) {
                    Log.e(TAG, "camera/encoder failed", e)
                    return reject(Protocol.STATUS_CAMERA_FAILED, context.getString(R.string.reject_camera, e.message ?: ""))
                }
                synchronized(out) { out.writeConfig(Protocol.STATUS_OK, enc.size.width, enc.size.height, enc.fps) }
                configSent = true
                pendingCaps?.let { sendControl(Protocol.T_CAPABILITIES, it) }
                publish(enc)

                lastRxMs = System.currentTimeMillis()
                Thread(::sendLoop, "acam-send").start()
                readLoop(input, enc)
            } catch (e: Exception) {
                // Any failure (network, camera, codec) ends only this session, never the app.
                if (!closed) Log.i(TAG, "session $peer ended: $e")
            } finally {
                close()
            }
        }

        private fun publish(enc: CameraEncoder) = onSession(
            Session(!requireToken, peer, enc.size.width, enc.size.height, enc.fps, enc.front),
        )

        private fun reject(status: Int, reason: String) {
            Log.w(TAG, "rejecting $peer: $reason")
            onMessage(context.getString(R.string.rejected, peer, reason))
            synchronized(out) { out.writeConfig(status, 0, 0, 0) }
        }

        private fun readLoop(input: DataInputStream, enc: CameraEncoder) {
            while (!closed) {
                val p = Packet.read(input)
                lastRxMs = System.currentTimeMillis()
                when (p.type) {
                    Protocol.T_PING -> send(Packet(Protocol.T_PONG, 0, p.ptsUs, ByteArray(0)))
                    Protocol.T_PONG -> {}
                    Protocol.T_SET_CONTROLS -> enc.setControls(Tlv.decode(p.payload))
                    Protocol.T_BYE -> {
                        Log.i(TAG, "$peer closed the session")
                        close()
                    }
                    Protocol.T_REQ_KEYFRAME -> enc.requestKeyframe()
                    Protocol.T_SET_BITRATE -> if (p.payload.size == 4) {
                        enc.setBitrate(DataInputStream(p.payload.inputStream()).readInt())
                    }
                    Protocol.T_SWITCH_CAMERA -> {
                        queue.clear()
                        codecConfig = null
                        enc.switchCamera()
                        publish(enc)
                    }
                    else -> Log.w(TAG, "packet 0x${p.type.toString(16)} ignored")
                }
            }
        }

        private fun sendLoop() {
            try {
                while (!closed) {
                    if (System.currentTimeMillis() - lastRxMs > IDLE_TIMEOUT_MS) {
                        Log.i(TAG, "$peer idle for more than ${IDLE_TIMEOUT_MS / 1000} s, closing the session")
                        close()
                        break
                    }
                    val p = queue.poll(500, TimeUnit.MILLISECONDS) ?: continue
                    send(p)
                }
            } catch (e: IOException) {
                if (!closed) Log.i(TAG, "send to $peer failed: $e")
                close()
            } catch (_: InterruptedException) {}
        }

        private fun send(p: Packet) = synchronized(out) { p.writeTo(out) }

        override fun onCodecConfig(data: ByteArray) {
            codecConfig = data
            enqueue(Packet(Protocol.T_CODEC_CONFIG, 0, 0, data))
        }

        override fun onCapabilities(caps: Map<Int, Long>) {
            // The first camera start happens before CONFIG; hold the capabilities until it's sent.
            if (configSent) sendControl(Protocol.T_CAPABILITIES, caps) else pendingCaps = caps
        }

        override fun onCameraState(state: Map<Int, Long>) {
            if (configSent) sendControl(Protocol.T_CAMERA_STATE, state)
        }

        /** Small metadata packets skip the frame queue, so they're never dropped with a backlog. */
        private fun sendControl(type: Int, tlv: Map<Int, Long>) {
            if (closed) return
            try {
                send(Packet(type, 0, 0, Tlv.encode(tlv)))
            } catch (e: IOException) {
                close()
            }
        }

        override fun onFrame(data: ByteArray, keyframe: Boolean, ptsUs: Long) {
            if (dropUntilKeyframe) {
                if (!keyframe) return
                dropUntilKeyframe = false
                // The decoder on the PC is reset by CODEC_CONFIG, so resend it ahead of the keyframe.
                codecConfig?.let { queue.offer(Packet(Protocol.T_CODEC_CONFIG, 0, 0, it)) }
            }
            enqueue(Packet(Protocol.T_FRAME, if (keyframe) Protocol.FLAG_KEYFRAME else 0, ptsUs, data))
        }

        private fun enqueue(p: Packet) {
            if (queue.offer(p)) return
            // Network can't keep up: drop the backlog and resync on a fresh keyframe.
            queue.clear()
            if (p.type == Protocol.T_CODEC_CONFIG) {
                queue.offer(p)
            } else {
                dropUntilKeyframe = true
                encoder?.requestKeyframe()
            }
        }

        @Synchronized
        fun close() {
            if (closed) return
            closed = true
            try { closer() } catch (_: IOException) {}
            encoder?.release()
            encoder = null
            queue.clear()
            val wasCurrent = synchronized(sessions) {
                sessions -= this
                (current === this).also { if (it) current = null }
            }
            if (wasCurrent) onSession(null)
            // Called here rather than when run() returns: a blocked accessory read may outlive close().
            onEnd()
        }
    }

    companion object {
        private const val TAG = "AcamServer"
        // ~0.5 s at 30 fps.
        private const val QUEUE_FRAMES = 15
        // The PC pings every 2 s.
        private const val IDLE_TIMEOUT_MS = 6000L
    }
}
