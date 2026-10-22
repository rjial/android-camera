package com.acam.camera

import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.IOException

/** ACAM v1 wire format. See PROTOCOL.md at the repository root. */
object Protocol {
    val MAGIC = byteArrayOf('A'.code.toByte(), 'C'.code.toByte(), 'A'.code.toByte(), 'M'.code.toByte())
    const val VERSION = 1
    const val TCP_PORT = 5000
    const val DISCOVERY_PORT = 5001
    const val MAX_PAYLOAD = 8 * 1024 * 1024

    const val T_CODEC_CONFIG = 0x01
    const val T_FRAME = 0x02
    const val T_REQ_KEYFRAME = 0x10
    const val T_SWITCH_CAMERA = 0x11
    const val T_SET_BITRATE = 0x12
    const val T_BYE = 0x13
    const val T_SET_CONTROLS = 0x14
    const val T_PING = 0x20
    const val T_PONG = 0x21
    const val T_CAPABILITIES = 0x22
    const val T_CAMERA_STATE = 0x23

    const val FLAG_KEYFRAME = 1

    const val STATUS_OK = 0
    const val STATUS_BAD_TOKEN = 1
    const val STATUS_BAD_VERSION = 2
    const val STATUS_CAMERA_FAILED = 3

    const val CODEC_H264 = 1
}

/** Keys for SET_CONTROLS / CAMERA_STATE. Mirror of pc/src/protocol.rs `ctl`. */
object Ctl {
    const val AE_MODE = 1
    const val ISO = 2
    const val EXPOSURE_NS = 3
    const val EV_STEPS = 4
    const val AE_LOCK = 5
    const val AF_MODE = 6
    const val FOCUS_MILLI_DIOPTER = 7
    const val AF_TRIGGER = 8
    const val AWB_MODE = 9
    const val WB_KELVIN = 10
    const val AWB_LOCK = 11
    const val ZOOM_MILLI = 12
    const val ERROR = 99
}

/** Keys for CAPABILITIES. Mirror of pc/src/protocol.rs `cap`. */
object Cap {
    const val MANUAL_SENSOR = 100
    const val ISO_MIN = 101
    const val ISO_MAX = 102
    const val EXPOSURE_MIN_NS = 103
    const val EXPOSURE_MAX_NS = 104
    const val EV_MIN = 105
    const val EV_MAX = 106
    const val EV_STEP_MILLI = 107
    const val MIN_FOCUS_MILLI_DIOPTER = 108
    const val AWB_MODES = 109
    const val MANUAL_WB = 110
    const val ZOOM_MIN_MILLI = 111
    const val ZOOM_MAX_MILLI = 112
    const val AE_LOCK = 113
    const val AWB_LOCK = 114
    const val FRAME_DURATION_NS = 115
    const val AUTOFOCUS = 116
}

/** `key u8 | value i64` pairs. */
object Tlv {
    fun encode(pairs: Map<Int, Long>): ByteArray {
        val buf = java.nio.ByteBuffer.allocate(pairs.size * 9)
        pairs.forEach { (k, v) -> buf.put(k.toByte()).putLong(v) }
        return buf.array()
    }

    fun decode(b: ByteArray): Map<Int, Long> {
        val buf = java.nio.ByteBuffer.wrap(b)
        val out = LinkedHashMap<Int, Long>()
        while (buf.remaining() >= 9) out[buf.get().toInt() and 0xff] = buf.getLong()
        return out
    }
}

class Hello(
    val version: Int,
    val width: Int,
    val height: Int,
    val fps: Int,
    val bitrateKbps: Int,
    val camera: Int,
    val token: Int,
) {
    companion object {
        fun read(input: DataInputStream): Hello {
            syncToMagic(input)
            return Hello(
                version = input.readUnsignedByte(),
                width = input.readUnsignedShort(),
                height = input.readUnsignedShort(),
                fps = input.readUnsignedByte(),
                bitrateKbps = input.readInt(),
                camera = input.readUnsignedByte(),
                token = input.readInt(),
            )
        }
    }
}

/**
 * Skips bytes until "ACAM" has been read. Over USB the pipe can still hold packets from a previous
 * PC session (e.g. pings) when a new handshake starts.
 */
fun syncToMagic(input: DataInputStream) {
    val m = Protocol.MAGIC
    var matched = 0
    var skipped = 0
    while (matched < m.size) {
        val b = input.readUnsignedByte().toByte()
        matched = when {
            b == m[matched] -> matched + 1
            b == m[0] -> 1
            else -> 0
        }
        if (++skipped > 1_000_000) throw IOException("bukan klien ACAM")
    }
}

fun DataOutputStream.writeConfig(status: Int, width: Int, height: Int, fps: Int) {
    write(Protocol.MAGIC)
    writeByte(Protocol.VERSION)
    writeByte(status)
    writeShort(width)
    writeShort(height)
    writeByte(fps)
    writeByte(Protocol.CODEC_H264)
    flush()
}

class Packet(val type: Int, val flags: Int, val ptsUs: Long, val payload: ByteArray) {
    val isKeyframe get() = flags and Protocol.FLAG_KEYFRAME != 0

    fun writeTo(out: DataOutputStream) {
        out.writeByte(type)
        out.writeByte(flags)
        out.writeInt(payload.size)
        out.writeLong(ptsUs)
        out.write(payload)
        out.flush()
    }

    companion object {
        fun read(input: DataInputStream): Packet {
            val type = input.readUnsignedByte()
            val flags = input.readUnsignedByte()
            val len = input.readInt()
            if (len < 0 || len > Protocol.MAX_PAYLOAD) throw IOException("payload $len byte tidak valid")
            val pts = input.readLong()
            val payload = ByteArray(len)
            input.readFully(payload)
            return Packet(type, flags, pts, payload)
        }
    }
}
