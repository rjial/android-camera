# TODO

## Windows and macOS build targets

The `acam` PC app and the OBS plugin are Linux-only for now. The Android app is not affected.
Already cross-platform: the GUI (gpui + gpui-component), H.264 decoding (openh264), the Wi-Fi connection (TCP/UDP),
translations and camera controls. What is still Linux-specific is listed below.

### Shared (Windows and macOS)

- [ ] Put Linux-only code behind `#[cfg(target_os = "linux")]`: `v4l2out.rs`, `libc::clock_gettime`/`getuid`
      in `share.rs`, and the `$XDG_RUNTIME_DIR` socket path.
- [ ] Turn the virtual webcam output into a trait (`VirtualCamera`) with one implementation per OS; v4l2loopback becomes
      the Linux implementation.
- [x] Transport to the OBS plugin: unix socket on Linux/macOS, named pipe `\\.\pipe\acam-video` on Windows
      (`share.rs`, `obs-plugin/src/acam-source.c`). The `ACVF` format stays the same.
- [ ] The `pts_ns` clock must match OBS's `os_gettime_ns()`: done for Windows (QPC); `mach_absolute_time` on macOS.
- [ ] Location of the saved language file: `%APPDATA%\acam\language` (Windows), `~/Library/Application Support/acam/language` (macOS).
- [ ] Add Windows and macOS jobs to `.github/workflows/ci.yml` and `release.yml` (matrix), with their artifact names.
- [ ] Test USB (Android Open Accessory) through libusb on both OSes.

### Windows

**Status: a working prototype exists on branch `windows-vcam-wip` (not merged).** The virtual camera
shows live phone video in OBS (also through the ACAM Camera source), the Windows Camera app and browsers,
without Administrator. GPU decode fixed the latency spiral on constrained hardware. What's left before merge:
check the OBS plugin still builds on Linux, then packaging (installer, CI, release).

What works on the branch:
- [x] Foundation: Linux-only code behind `#[cfg(target_os = "linux")]`; `VirtualCamera` trait (`pc/src/webcam.rs`)
      with v4l2loopback as the Linux impl; cross-platform clock; `%APPDATA%\acam\language`; per-OS default device.
      Builds on `x86_64-pc-windows-msvc`, `cargo test` green.
- [x] Virtual webcam (Windows 11): `MFCreateVirtualCamera` with our own COM media source DLL
      (`pc/vcam-source`), registered via `scripts/windows-vcam-register.ps1`. Shows up and streams.
- [x] Real frames producer→DLL over `pc/vcam-ipc` (pagefile-backed `Global\` shared-memory section, seqlock,
      fixed 1280x720 NV12). **No elevation needed:** the DLL (in the Frame Server, which has
      `SeCreateGlobalPrivilege`) creates the section when an app opens the camera, with a DACL that lets
      interactive users write; a normal `acam.exe` keeps retrying to attach (every 200 ms) and then keeps its
      handle. Verified by `frames_reach_camera_without_admin` (ignored test in `vcam_windows.rs`). Only the
      one-time DLL registration (`regsvr32`, HKLM) still needs Administrator.
- [x] USB: `acam --adb` (and the GUI's ADB tab) runs `adb forward tcp:5000 tcp:5000` itself on every
      (re)connect and tunnels acam's existing TCP over the cable (scrcpy's approach — no AOA driver needed;
      `pc/src/adb.rs`). An AOA WinUSB INF is in `pc/accessory-driver/` as an alternative, but AOA-direct on
      Windows is fiddly (see struggles).
- [x] One process: `acam.exe` creates the "ACAM Camera" device itself (`MFCreateVirtualCamera`, session
      lifetime, current user — no elevation for this part) when the webcam output opens, and removes it when
      the output closes or acam exits. `create_camera.exe` is now only a test harness for the DLL alone.
      (Pattern confirmed against OpenCam's `OpenCamVirtualCameraHost.cpp`.)
- [x] Hardware decode: on Windows the Media Foundation H.264 decoder MFT is used with a D3D11 device (DXVA,
      GPU), falling back to MF software, then openh264 (`pc/src/decode_mf.rs`, `pc/src/decode.rs`).
      `MF_LOW_LATENCY` keeps it at one frame out per frame in. A GPU decoder that fails mid-stream is
      replaced by a software one at the next keyframe. `ACAM_DECODER=openh264|mf|mf-sw` forces a choice.
      720p on the dev laptop: openh264 3.7 ms/frame, MF CPU 2.2 ms, MF GPU 1.8 ms (and the CPU stays free).

Still to do:
- [ ] **Latency / graceful degradation.** On a thermally/memory-constrained laptop, sustained software
      H.264 decode throttled the whole CPU and latency spiralled once acam fell behind (decode climbed
      18ms→167ms over a session; our output path is only ~0.6ms). **GPU decode fixed it** (measured with a
      phone over `--adb`, 720p30 ~4 Mbps, ~6 min): rtt stays 2–40 ms with brief self-recovering spikes, 30 fps.
      `decode` (incl. the GPU→RAM readback in `Lock2D`) still drifts 10→~28 ms and then plateaus — under the
      33 ms frame budget, but with little headroom. Ideas: overlap the readback with the next decode, or keep
      frames on the GPU. Still worth adding drop-stale-frames to the engine so latency stays bounded on
      hardware without a usable GPU decoder.
- [x] Removed the temporary decode/deliver timing from `engine.rs::receive` (the stats line is fps, kbps
      and rtt again). To measure decode speed now: `cargo test --release --bin acam decoder_speed -- --ignored --nocapture`.
- [ ] Windows 10 has no `MFCreateVirtualCamera` (acam reports it and points at the OBS plugin). A DirectShow
      source filter (what `dimalinau-lab/Virtual-Camera` does) would cover Win10 and DirectShow-only apps, at
      the cost of not showing in the Windows Camera app — only if Win10 users ask.
- [ ] Sign the accessory WinUSB INF (`pc/accessory-driver/`) so it installs without disabling driver-signature
      enforcement; or drop the AOA path entirely in favour of `--adb`.
- [ ] UDP discovery: Windows Firewall permission for ports 5000/5001 on first run.
- [ ] Build: `.exe` icon and metadata, then an installer (MSI via `cargo-wix`, or a zip) that also installs +
      registers the vcam-source DLL.
- [x] OBS plugin: `obs-plugin/build-windows.ps1` builds `acam-source.dll` with MSVC against the installed OBS
      (libobs headers fetched for that exact version, import lib generated from `obs.dll`) and installs it to
      `%ProgramData%\obs-studio\plugins\` without admin. Verified loading in OBS 32.2.2. acam serves it over
      `\\.\pipe\acam-video` (user-only DACL). Still to do: build it in CI and ship it in the release.

#### Struggles / hard-won lessons (so the next person doesn't repeat them)

- **`MFCreateVirtualCamera` needs a whole custom COM media source, not a "push a frame" API.** The debugging
  chain was: the registered CLSID must hand out an **`IMFActivate`** (not `IMFMediaSource`) from
  `IClassFactory::CreateInstance`; its `ActivateObject` must return an **`IMFMediaSourceEx`** (not plain
  `IMFMediaSource`); `GetSourceAttributes` must return a **persistent** store carrying
  `MF_DEVICEMFT_SENSORPROFILE_COLLECTION` (a real `IMFSensorProfileCollection` with a `KSCAMERAPROFILE_Legacy`
  profile + `AddProfileFilter(0, "((RES==;FRT<=30,1;SUT==))")` — the filter-string syntax is only in
  Microsoft's "Camera Profile V2 Device MFT Support" docs); stream attributes need
  `MF_DEVICESTREAM_STREAM_CATEGORY`/`_STREAM_ID`/`_FRAMESERVER_SHARED`/`_ATTRIBUTE_FRAMESOURCE_TYPES`. Each was
  a separate `E_NOINTERFACE`/`E_INVALIDARG`/`MF_E_ATTRIBUTENOTFOUND` found by logging the exact IID/attr the
  caller asked for and matching it against the SDK headers. `smourier/VCamSample` (C++) was the key reference.
- **`E_ACCESSDENIED` on `IMFVirtualCamera::Start` = the DLL is somewhere the Frame Server services (Local
  Service / Local System) can't read.** Don't register it from a build dir under `C:\Users\<you>\...`; install
  it to `C:\Users\Public\acam\` (world-readable, user-writable).
- **Black screen despite frames flowing = zero-based sample timestamps.** A live source must stamp samples with
  `MFGetSystemTime()`; a renderer whose clock is already at wall-clock time drops every "past" frame.
- **AOA-over-USB on Windows is a driver rabbit hole.** With USB debugging on, the phone enumerates as the
  accessory+ADB composite (PID `2D01`) and Windows binds the **MTP driver to interface 0** (the accessory data
  interface libusb needs) → `Entity not found`. Zadig targets the wrong interface and hits "Resource already
  exists". With debugging off (PID `2D00`, cleaner) libusb can't open the phone to *send* the AOA switch.
  Conclusion: `adb forward` (what scrcpy/most tools actually use on Windows) is far more robust — and ACAM's
  existing TCP server works through it unchanged.
- **The latency rabbit hole.** Early symptom was huge, growing latency. It was NOT our code:
  - Over Wi-Fi it was bufferbloat (rtt grew to 8s as the phone outran the link).
  - Over a USB `adb forward` tunnel, `--no-webcam` gave 2ms rtt but webcam-on grew unbounded → we blamed the
    output path. A disk-backed mmap file (`video.bin`) *did* add a real stall (writeback on a 92%-full SSD), so
    we moved to pagefile-backed RAM shared memory — but the latency still spiralled.
  - Per-frame timing finally showed the truth: `deliver` (our scale + NV12 + shared-mem write) is ~0.6ms;
    **decode** is ~18ms and *climbs with session time* (→167ms), with `deliver` rising proportionally — i.e.
    whole-CPU throttling/swap, a hardware limit, made worse once acam decodes a backlog flat-out and spirals.
  - Method that isolates this cleanly: `acam --headless --adb` (the USB tunnel takes the network out of the
    picture), and watch `rtt` in the stats line. The per-frame `decode`/`deliver` timing that found the cause
    was temporary instrumentation in `engine.rs::receive` (removed; see commit 8bfc13e and earlier for it).

### macOS

- [ ] Virtual webcam: a CoreMediaIO Camera Extension (macOS 12.3+). Needs an app bundle, the system extension
      entitlement and a Developer ID for signing; this is the largest piece.
- [ ] USB: AOA through libusb should work without a driver; make sure it does not clash with Android File Transfer.
- [ ] Build: universal binary (`aarch64-apple-darwin` + `x86_64-apple-darwin`, via `lipo`), an `.app` bundle,
      then signing and notarization (`.dmg`).
- [ ] OBS plugin: build a universal `.plugin` bundle with the macOS OBS SDK, install to
      `~/Library/Application Support/obs-studio/plugins/`.
- [ ] Permissions: macOS 15 asks for "Local Network" permission for UDP discovery.
