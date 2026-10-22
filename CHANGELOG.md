# Changelog

Semua perubahan penting dicatat di sini. Formatnya mengikuti [Keep a Changelog](https://keepachangelog.com/id-ID/1.1.0/),
dan versinya mengikuti [Semantic Versioning](https://semver.org/lang/id/). Satu nomor versi berlaku untuk ketiga
bagian (aplikasi PC `acam`, aplikasi Android, dan plugin OBS), karena ketiganya berbicara dengan protokol yang sama.

Selama masih `0.x`, protokol ACAM dan format socket ke OBS boleh berubah di rilis minor. Mulai `1.0.0`, perubahan
yang tidak kompatibel hanya terjadi di rilis major.

## [0.1.0] - 2026-10-23

Rilis publik pertama.

### Added

- Aplikasi Android: kamera HP sebagai webcam PC lewat Wi-Fi (TCP + token pairing, discovery UDP) atau kabel USB
  (Android Open Accessory, tanpa ADB).
- Aplikasi PC `acam` untuk Linux, berupa GUI (preview, panel koneksi dan kamera) atau `--headless`. Output ke webcam
  virtual v4l2loopback, dan bisa reconnect otomatis.
- Kontrol kamera manual yang mengikuti kemampuan HP: ISO, shutter (sampai 1 detik, termasuk 1/24 dan 1/25),
  EV, AE lock, fokus manual, white balance (preset, Kelvin, AWB lock), dan zoom.
- Plugin OBS "ACAM Camera" yang mengambil video langsung dari acam lewat unix socket, tanpa v4l2loopback.
- Gambar NO SIGNAL ala TV dengan penyebabnya (sinyal hilang, HP terputus, berbagi dimatikan, acam tidak berjalan),
  di webcam virtual dan di OBS.
- Bahasa English dan Bahasa Indonesia di ketiga bagian, dengan pemilih bahasa di GUI.

[0.1.0]: https://github.com/rjial/android-camera/releases/tag/v0.1.0
