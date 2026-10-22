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
- [ ] Replace the transport to the OBS plugin: unix socket on Linux/macOS, named pipe `\\.\pipe\acam-video` on Windows
      (or AF_UNIX, available since Windows 10 1803). The `ACVF` format stays the same.
- [ ] The `pts_ns` clock must match OBS's `os_gettime_ns()`: `QueryPerformanceCounter` on Windows,
      `mach_absolute_time` on macOS.
- [ ] Location of the saved language file: `%APPDATA%\acam\language` (Windows), `~/Library/Application Support/acam/language` (macOS).
- [ ] Add Windows and macOS jobs to `.github/workflows/ci.yml` and `release.yml` (matrix), with their artifact names.
- [ ] Test USB (Android Open Accessory) through libusb on both OSes.

### Windows

- [ ] Virtual webcam: a Media Foundation virtual camera (`MFCreateVirtualCamera`, Windows 11) as the main option;
      Windows 10 needs a DirectShow filter (or point users to the OBS Virtual Camera through the plugin).
- [ ] USB: Windows does not let libusb open the phone without a WinUSB driver. Document Zadig, or ship a driver
      installer for the accessory VID/PID `18d1:2d00`/`2d01`.
- [ ] UDP discovery: Windows Firewall permission for ports 5000/5001 on first run.
- [ ] Build: target `x86_64-pc-windows-msvc`, `.exe` icon and metadata, then an installer (MSI via `cargo-wix`, or a zip).
- [ ] OBS plugin: build a `.dll` with the Windows OBS SDK (obs-deps + obs-studio), install to
      `%ProgramData%\obs-studio\plugins\acam-source\`.

### macOS

- [ ] Virtual webcam: a CoreMediaIO Camera Extension (macOS 12.3+). Needs an app bundle, the system extension
      entitlement and a Developer ID for signing; this is the largest piece.
- [ ] USB: AOA through libusb should work without a driver; make sure it does not clash with Android File Transfer.
- [ ] Build: universal binary (`aarch64-apple-darwin` + `x86_64-apple-darwin`, via `lipo`), an `.app` bundle,
      then signing and notarization (`.dmg`).
- [ ] OBS plugin: build a universal `.plugin` bundle with the macOS OBS SDK, install to
      `~/Library/Application Support/obs-studio/plugins/`.
- [ ] Permissions: macOS 15 asks for "Local Network" permission for UDP discovery.
