# ACAM v1

H.264 streaming from an Android camera to a PC over TCP. All numbers are big-endian.

- Phone = TCP **server**, default port `5000`.
- PC = **client**.
- Over USB, the same byte stream runs on the Android Open Accessory bulk endpoints
  (identity: manufacturer `ACAM`, model `ACAM Webcam`). The token is ignored, because plugging in the cable
  already counts as pairing.
- Discovery (optional): the phone broadcasts UDP to port `5001` every second with the payload
  `"ACAM" | ver u8 | tcp_port u16 | name_len u8 | name[name_len]` (UTF-8).

## Handshake

Client → server (19 bytes):

| field        | type  | notes                               |
|--------------|-------|-------------------------------------|
| magic        | 4B    | `"ACAM"`                            |
| version      | u8    | `1`                                 |
| width        | u16   | requested resolution                |
| height       | u16   |                                     |
| fps          | u8    |                                     |
| bitrate_kbps | u32   |                                     |
| camera       | u8    | `0` = back, `1` = front             |
| token        | u32   | pairing token shown on the phone    |

Server → client (12 bytes):

| field   | type | notes                                                               |
|---------|------|---------------------------------------------------------------------|
| magic   | 4B   | `"ACAM"`                                                            |
| version | u8   | `1`                                                                 |
| status  | u8   | `0` OK, `1` wrong token, `2` unsupported version, `3` camera failed |
| width   | u16  | resolution actually used by the encoder                             |
| height  | u16  |                                                                     |
| fps     | u8   |                                                                     |
| codec   | u8   | `1` = H.264                                                         |

If `status != 0`, the server closes the connection.

## Packets

After the handshake, both directions use the same framing:

```
type u8 | flags u8 | len u32 | pts_us u64 | payload[len]
```

The header is 14 bytes. `len` is at most 8 MiB.

| type   | direction | payload / meaning |
|--------|-----------|-------------------|
| `0x01` | phone → PC | `CODEC_CONFIG`: SPS+PPS in Annex-B. Always sent before the first frame |
| `0x02` | phone → PC | `FRAME`: one H.264 Annex-B access unit. `flags & 1` = keyframe |
| `0x10` | PC → phone | `REQ_KEYFRAME`: no payload |
| `0x11` | PC → phone | `SWITCH_CAMERA`: no payload, toggles front/back camera |
| `0x12` | PC → phone | `SET_BITRATE`: payload `u32` kbps |
| `0x13` | PC → phone | `BYE`: the PC ends the session; the phone releases the camera at once |
| `0x14` | PC → phone | `SET_CONTROLS`: TLV of changed camera controls (see below) |
| `0x22` | phone → PC | `CAPABILITIES`: TLV of the active camera's capabilities; sent after CONFIG and after camera switches |
| `0x23` | phone → PC | `CAMERA_STATE`: TLV of actual values (ISO, shutter, focus, zoom), at most 4×/second |
| `0x20` | both | `PING`: `pts_us` = sender time |
| `0x21` | both | `PONG`: `pts_us` copied from the PING |

One client is active at a time. When a new client connects, the old one is dropped. A connection with no data
for 6 seconds is considered dead. After `SWITCH_CAMERA` the server sends a new `CODEC_CONFIG` and the resolution
may change, so clients read the resolution from the SPS/decoded frames, not only from the handshake.

## Camera controls (TLV)

The `SET_CONTROLS`, `CAPABILITIES` and `CAMERA_STATE` payloads contain repeated pairs of
`key u8 | value i64`. Unknown keys are ignored. A truncated trailing pair is ignored too.

Features depend on the phone and camera. The phone only reports what the active camera really supports;
a missing capability key means the feature is unavailable. The phone still clamps or ignores unsupported
values, so a client cannot put the camera into an error state.

### Control keys (`SET_CONTROLS`, also used by `CAMERA_STATE`)

| key | name | value |
|-----|------|-------|
| 1 | AE_MODE | 0 auto, 1 manual (ISO + shutter) |
| 2 | ISO | sensitivity |
| 3 | EXPOSURE_NS | exposure time (max 1 s); longer than one frame lowers the stream fps |
| 4 | EV_STEPS | exposure compensation in steps (see the EV step capability) |
| 5 | AE_LOCK | 0/1 |
| 6 | AF_MODE | 0 continuous, 1 manual |
| 7 | FOCUS_MILLI_DIOPTER | manual focus distance; 0 = ∞, 1000 = 1 m |
| 8 | AF_TRIGGER | 1 = focus once, then hold |
| 9 | AWB_MODE | Camera2 `CONTROL_AWB_MODE_*` (1 auto, 2 incandescent, 3 fluorescent, 4 warm fluorescent, 5 daylight, 6 cloudy, 7 twilight, 8 shade); 0 = manual Kelvin |
| 10 | WB_KELVIN | 2000–10000; approximate, relative to auto white balance |
| 11 | AWB_LOCK | 0/1 |
| 12 | ZOOM_MILLI | 1000 = 1× |
| 99 | ERROR | `CAMERA_STATE` only: the phone rejected the last combination and kept the previous settings |

### Capability keys (`CAPABILITIES`)

| key | meaning |
|-----|---------|
| 100 | manual ISO/shutter supported (0/1) |
| 101 / 102 | ISO min / max |
| 103 / 104 | exposure min / max (ns) |
| 105 / 106 | EV min / max (steps) |
| 107 | size of one EV step ×1000 |
| 108 | closest focus (milli-diopters); 0 = no manual focus |
| 109 | AWB preset bitmask (bit n = `CONTROL_AWB_MODE` n) |
| 110 | manual Kelvin white balance supported (0/1) |
| 111 / 112 | zoom min / max ×1000 |
| 113 / 114 | AE lock / AWB lock available (0/1) |
| 115 | duration of one frame (ns) at the requested fps |
| 116 | autofocus available (0/1) |

## Sharing with OBS (`ACVF`)

Local protocol between `acam` (server) and the OBS source plugin "ACAM Camera" (client), one-way from acam to the plugin.

- **Socket:** unix stream at `$ACAM_SOCKET`, else `$XDG_RUNTIME_DIR/acam/video.sock`, else
  `/tmp/acam-<uid>/video.sock`. The folder is created with mode 0700 and the socket with 0600, so only the same user can read it.
  On **Windows** it is a byte-mode named pipe at `$ACAM_SOCKET`, else `\\.\pipe\acam-video` (one instance per client, local
  clients only, DACL: the same user and SYSTEM).
- **Frame:** a 24-byte little-endian header, then tightly packed I420 (`w·h` bytes of Y, then `w/2·h/2` bytes of U, then V).

| offset | size | content |
|--------|------|---------|
| 0 | 4 | magic `ACVF` |
| 4 | 4 | width (u32) |
| 8 | 4 | height (u32) |
| 12 | 8 | ns when acam sent it (u64) on OBS's `os_gettime_ns()` clock (`CLOCK_MONOTONIC`; QueryPerformanceCounter on Windows), for measuring latency |
| 20 | 1 | color: 0 = BT.601, 1 = BT.709 |
| 21 | 1 | range: 0 = limited |
| 22 | 1 | idle frames only: reason (0 = phone disconnected, 1 = signal lost, 2 = OBS sharing off) |
| 23 | 1 | padding |

- **Idle:** `width = height = 0` with no payload means there is no video; byte 22 gives the reason and the plugin shows
  the NO SIGNAL picture. acam resends it every 500 ms while there is no video, so newly attached sources get it too.
- A slow client loses frames (a queue of 2 frames per client) and never holds up acam's pipeline.
- A client drops the connection if the magic is wrong or the size is > 8192 or odd, then reconnects.
