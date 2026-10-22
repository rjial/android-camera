# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/). One version number covers all three parts
(the `acam` PC app, the Android app and the OBS plugin), because they speak the same protocol.

While on `0.x`, the ACAM protocol and the OBS socket format may change in a minor release. From `1.0.0` on,
incompatible changes only happen in a major release.

## [Unreleased]

### Added

- MIT license.

### Changed

- Documentation (README, CHANGELOG, PROTOCOL, TODO) is now in English.

## [0.1.0] - 2026-10-23

First public release.

### Added

- Android app: the phone camera as a PC webcam over Wi-Fi (TCP with a pairing token, UDP discovery) or a USB cable
  (Android Open Accessory, no ADB).
- `acam` PC app for Linux, as a GUI (preview, connection and camera panel) or `--headless`. Outputs to a v4l2loopback
  virtual webcam and reconnects automatically.
- Manual camera controls that follow the phone's capabilities: ISO, shutter (up to 1 second, including 1/24 and 1/25),
  EV, AE lock, manual focus, white balance (presets, Kelvin, AWB lock) and zoom.
- "ACAM Camera" OBS source plugin that takes video straight from acam over a unix socket, without v4l2loopback.
- TV-style NO SIGNAL picture with the cause (signal lost, phone disconnected, sharing off, acam not running),
  on the virtual webcam and in OBS.
- English and Indonesian in all three parts, with a language picker in the GUI.

[Unreleased]: https://github.com/rjial/android-camera/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/rjial/android-camera/releases/tag/v0.1.0
