# TODO

## Build target Windows dan macOS

Saat ini aplikasi PC `acam` dan plugin OBS hanya untuk Linux. Aplikasi Android tidak terpengaruh.
Yang sudah lintas platform: GUI (gpui + gpui-component), decode H.264 (openh264), koneksi Wi-Fi (TCP/UDP), terjemahan,
dan kontrol kamera. Yang masih khusus Linux tercantum di bawah.

### Bersama (Windows dan macOS)

- [ ] Pisahkan kode khusus Linux di balik `#[cfg(target_os = "linux")]`: `v4l2out.rs`, `libc::clock_gettime`/`getuid`
      di `share.rs`, dan path socket `$XDG_RUNTIME_DIR`.
- [ ] Jadikan output webcam virtual sebuah trait (`VirtualCamera`) dengan satu implementasi per OS; v4l2loopback jadi
      implementasi Linux.
- [ ] Ganti transport ke plugin OBS: unix socket di Linux/macOS, named pipe `\\.\pipe\acam-video` di Windows
      (atau AF_UNIX, yang ada sejak Windows 10 1803). Format `ACVF` tetap sama.
- [ ] Jam untuk `pts_ns` harus sama dengan `os_gettime_ns()` OBS: `QueryPerformanceCounter` di Windows,
      `mach_absolute_time` di macOS.
- [ ] Lokasi file bahasa yang disimpan: `%APPDATA%\acam\language` (Windows), `~/Library/Application Support/acam/language` (macOS).
- [ ] Tambah job Windows dan macOS di `.github/workflows/ci.yml` dan `release.yml` (matrix), beserta nama artefaknya.
- [ ] Uji USB (Android Open Accessory) lewat libusb di kedua OS.

### Windows

- [ ] Webcam virtual: Media Foundation virtual camera (`MFCreateVirtualCamera`, Windows 11) sebagai pilihan utama;
      untuk Windows 10 perlu filter DirectShow (atau arahkan pengguna ke OBS Virtual Camera lewat plugin).
- [ ] USB: Windows tidak mengizinkan libusb membuka HP tanpa driver WinUSB. Dokumentasikan Zadig, atau sediakan
      installer driver untuk VID/PID accessory `18d1:2d00`/`2d01`.
- [ ] Discovery UDP: izin Windows Firewall untuk port 5000/5001 saat pertama kali dijalankan.
- [ ] Build: target `x86_64-pc-windows-msvc`, ikon dan metadata `.exe`, lalu installer (MSI lewat `cargo-wix`, atau zip).
- [ ] Plugin OBS: build `.dll` dengan SDK OBS Windows (obs-deps + obs-studio), install ke
      `%ProgramData%\obs-studio\plugins\acam-source\`.

### macOS

- [ ] Webcam virtual: Camera Extension CoreMediaIO (macOS 12.3+). Butuh app bundle, entitlement system extension,
      dan Developer ID untuk signing; ini bagian terbesar.
- [ ] USB: AOA lewat libusb seharusnya jalan tanpa driver; pastikan tidak bentrok dengan Android File Transfer.
- [ ] Build: universal binary (`aarch64-apple-darwin` + `x86_64-apple-darwin`, lewat `lipo`), `.app` bundle,
      lalu signing dan notarization (`.dmg`).
- [ ] Plugin OBS: build `.plugin` bundle (universal) dengan SDK OBS macOS, install ke
      `~/Library/Application Support/obs-studio/plugins/`.
- [ ] Izin kamera/jaringan lokal: macOS 15 meminta izin "Local Network" untuk discovery UDP.
