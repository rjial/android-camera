# ACAM: use an Android phone camera as a Linux webcam over Wi-Fi or USB

- `android/`: Kotlin app. Runs a TCP server on port 5000 and accepts USB connections (Android Open Accessory), encodes the camera to H.264, and announces itself on UDP 5001.
- `pc/`: Rust app `acam` with a GUI (gpui): camera preview, Wi-Fi/USB connection panel, camera controls. Decodes H.264 (openh264) and writes to v4l2loopback. A CLI mode is available with `--headless`.
- The Android UI uses Jetpack Compose (no camera preview on the phone; the preview is on the PC).
- `obs-plugin/`: OBS source plugin "ACAM Camera" (C) that takes video from `acam` without v4l2loopback.
- `PROTOCOL.md`: protocol specification. `CHANGELOG.md`: release history. `TODO.md`: roadmap (Windows, macOS).

Only Linux x86_64 is supported on the PC side for now; Windows and macOS support is tracked in [TODO.md](TODO.md).

## Install from a release

Download from [Releases](https://github.com/rjial/android-camera/releases/latest):

- **Phone:** `acam-<version>.apk`, then install it (allow "install unknown apps"). If you previously installed a
  self-built (debug) version, uninstall it first, because the signatures differ.
- **PC:** `acam-<version>-linux-x86_64.tar.gz`, then extract it and run `./acam`. Requires Vulkan and X11/Wayland.
- **OBS (optional):** `acam-obs-plugin-<version>-linux-x86_64.tar.gz`. Close OBS first, then:
  ```bash
  tar -xzf acam-obs-plugin-*-linux-x86_64.tar.gz -C ~/.config/obs-studio/plugins/
  ```

Verify the files with `sha256sum -c SHA256SUMS`.

## Build

Android app: open the `android/` folder in Android Studio and press Run, or from the command line:

```bash
./gradlew installDebug
```

PC app:

```bash
cd pc && cargo build --release
```

Dependency notes: `gpui 0.2.2` is paired with `gpui-component =0.5.1` (0.6+ uses a different gpui fork), and
`libc` is pinned to 0.2.188 in `Cargo.lock` because 0.2.190+ removed `ENOATTR`, which a gpui dependency (`xattr`) uses.
Do not run `cargo update` without `cargo update -p libc --precise 0.2.188` afterwards.
The GUI needs Vulkan (Mesa/NVIDIA driver) and X11 or Wayland.

OBS plugin (needs `obs-devel`/libobs headers and `cmake`). **Close OBS first**: overwriting a plugin that OBS has loaded crashes OBS.

```bash
cd obs-plugin && cmake -B build -DCMAKE_BUILD_TYPE=Release && cmake --build build && cmake --install build
```

The plugin is installed to `~/.config/obs-studio/plugins/acam-source/` (`bin/64bit/acam-source.so` plus translations in `data/locale/`).

## Usage

1. Load the v4l2loopback module (once per boot):
   ```bash
   sudo modprobe v4l2loopback devices=1 video_nr=10 card_label="Android Cam" exclusive_caps=1
   ```
2. On the phone, open ACAM and press **Start**. Note the IP and token shown.
3. On the PC, open the GUI: `pc/target/release/acam`. Pick Wi-Fi (click the phone in the "Phones found" list and enter the
   token) or USB, then **Connect**. The preview shows in the window; resolution, fps, bitrate, camera switching and the
   webcam output are set from the right-hand panel.

   Connection arguments connect right away when the GUI opens, e.g. `acam --usb`. Without a window (CLI):
   ```bash
   pc/target/release/acam --headless --host 192.168.1.23 --token 123456
   pc/target/release/acam --headless --discover --token 123456
   pc/target/release/acam --headless --usb
   pc/target/release/acam --headless --adb --token 123456
   ```
4. Pick the **Android Cam** camera in your browser, OBS or Zoom. Quick check: `ffplay /dev/video10`.

### Over a USB cable (no ADB, no network)

Pick the USB tab in the GUI, or run `acam --headless --usb`.

`acam` switches the phone into Android Open Accessory mode. The first time, the phone shows a dialog
"Open ACAM to handle ACAM Webcam?"; tick "always" and press OK. The app opens and streaming starts
automatically, without a token. The phone's USB mode (MTP/ADB) returns to normal once the cable is unplugged.

This needs access to the phone's USB device without root. The `android-udev-rules` package covers most brands.
If you get a permission error, add a udev rule for your phone's vendor and for `18d1:2d00`/`18d1:2d01`.
Unrecognised brands can be forced with `--usb-vid <hex>`.

### Over a USB cable through ADB

If you already have USB debugging on (Developer options) and Android platform-tools installed, pick the ADB tab in the
GUI or run `acam --headless --adb --token 123456`. `acam` runs `adb forward tcp:5000 tcp:5000` and connects through
the cable with the normal Wi-Fi protocol, so it needs the token but no udev rule or USB driver (the easiest USB route
on Windows). `adb` is looked up in `ACAM_ADB`, `ANDROID_HOME`/`ANDROID_SDK_ROOT`, the default SDK location,
`~/platform-tools` and `PATH`.

The virtual webcam is opened once with the size of the first connection and stays open across reconnects; streams at
other resolutions are scaled to that size, so apps like Zoom do not lose the camera.

### OBS plugin (without v4l2loopback)

In OBS: **Sources → + → ACAM Camera**. The source takes video from a running `acam` (GUI or `--headless`) over a local
unix socket, without the `v4l2loopback` module, so it keeps working with `--no-webcam` or with the virtual webcam switch off.
The picture follows the stream resolution, and OBS reconnects automatically when acam is restarted.
Several sources can be used at once. Camera controls are still set from acam.

The extra socket latency measures ~1 ms on average at 1080p30 (logged to the OBS log every minute, `[acam]` lines).
Sharing to OBS can be turned off with the **Share to OBS** switch in the GUI or with `--no-obs`. The default socket path is
`$XDG_RUNTIME_DIR/acam/video.sock`; override it with the `ACAM_SOCKET` variable (used by both acam and the plugin) or in the source properties.

### NO SIGNAL picture

When there is no video, OBS and the virtual webcam show TV-style colour bars with **NO SIGNAL** and the cause:

| caption | when |
|---|---|
| SIGNAL LOST | the phone is still connected but no frame arrived for 2.5 seconds (e.g. Wi-Fi stalls) |
| PHONE DISCONNECTED | cable unplugged, phone app closed, reconnecting, or disconnected in the GUI |
| OBS SHARING OFF | the **Share to OBS** switch is off or `--no-obs` is used (OBS only) |
| ACAM NOT RUNNING | the acam app is not running (OBS only, drawn by the plugin) |

The 2.5 second limit sits above the 1 second maximum shutter, so slow shutters do not trigger NO SIGNAL. A connection is
only declared lost after 6 seconds without data, so when the phone app is closed you see SIGNAL LOST, then PHONE DISCONNECTED.
In OBS the picture can be turned off in the source properties (**Show a NO SIGNAL picture…**); the source is then left empty.

### Camera controls

The **IMAGE** section of the GUI panel has exposure (auto + EV + AE lock, or manual ISO + shutter), focus
(auto, manual, refocus), white balance (presets, Kelvin, AWB lock) and zoom. Only the features supported by the
active camera of the connected phone are shown; front cameras, for example, usually lack manual ISO/focus. Settings
are kept across reconnects and reset to automatic when switching cameras. The shutter goes up to the sensor limit
(at most 1 second). A shutter longer than one frame lowers the stream fps (e.g. 1/25 at 30 fps → 25 fps);
to use 1/24 or 1/25 without that, pick 24/25 fps.

`--headless` commands (type, then Enter):

| command | effect |
|---|---|
| `s`, `k`, `b 2000` | switch camera, request a keyframe, bitrate 2000 kbps |
| `caps`, `status` | capabilities of the active camera, current ISO/shutter/focus values |
| `iso 800`, `iso auto` | manual ISO / back to auto |
| `shutter 1/250`, `shutter 20ms`, `shutter auto` | manual shutter |
| `ev -1.5` | exposure compensation |
| `focus 30` (cm), `focus inf`, `focus auto`, `refocus` | focus |
| `wb daylight`, `wb 3200K`, `wb auto` | white balance (cloudy, shade, incandescent, fluorescent, warm-fluorescent, twilight) |
| `zoom 2.5` | zoom |
| `lock ae on`, `lock awb off` | lock exposure / white balance |
| `reset` | everything back to automatic |

Other options: `--size 1920x1080`, `--fps 30`, `--bitrate 6000`, `--front`, `--dump file.h264`, `--no-webcam`, `--no-obs`, `--lang en|id`.
See `acam --help`.

## Languages

The PC app, the Android app and the OBS plugin are available in **English** and **Indonesian** (Bahasa Indonesia).
English is the fallback for untranslated text.

- **acam (PC):** pick a language in the **LANGUAGE** section of the GUI panel (applies immediately and is saved in
  `~/.config/acam/language`), or use `--lang id` / `ACAM_LANG=id`. Otherwise acam follows the system language
  (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`). Priority: `--lang` → `ACAM_LANG` → GUI choice → system language → English.
- **Android app:** follows the phone language. On Android 13+ it can be changed per app: Settings → Apps → ACAM → Language.
- **OBS plugin:** follows the OBS interface language (Settings → General → Language). OBS log lines are always in English.

The NO SIGNAL caption uses each output's language: the virtual webcam follows acam's language, the OBS source follows OBS.

### Adding a language

Each part uses its platform's standard translation format. Adding a language only takes new files, no code changes:

| part | copy from | to (German example) |
|---|---|---|
| acam (PC) | `pc/locales/en.yml` | `pc/locales/de.yml` |
| Android | `android/app/src/main/res/values/strings.xml` | `android/app/src/main/res/values-de/strings.xml` |
| OBS plugin | `obs-plugin/data/locale/en-US.ini` | `obs-plugin/data/locale/de-DE.ini` |

Rules:
- Translate the values only. Keys and placeholders (`%{name}` on the PC, `%1$s`/`%1$d` on Android) must stay unchanged.
- On the PC, `cargo test` checks that the new file has the same keys as `en.yml` and intact placeholders. The new language
  appears in the GUI language picker after a rebuild.
- On Android, the new language is added to the per-app language setting automatically (`generateLocaleConfig`). `./gradlew lintDebug` reports untranslated strings.
- `nosignal.*` / `NoSignal.*` texts are drawn with a built-in A–Z font, so only capital letters A–Z and spaces show.
  Latin accents (é, ñ, ü, …) are dropped automatically; non-Latin scripts (Cyrillic, CJK, Arabic, …) cannot be shown in that picture yet.

## Versioning and releases

This project uses [Semantic Versioning](https://semver.org/). One version in the `VERSION` file applies to the
PC app, the Android app (`versionName`; `versionCode` = major·10000 + minor·100 + patch) and the OBS plugin.
Bump **patch** for fixes, **minor** for new features and **major** for incompatible protocol changes
(while on `0.x`, protocol changes may happen in a minor release).

Releasing a new version:

```bash
scripts/version.sh bump 0.2.0      # updates VERSION, pc/Cargo.toml, Cargo.lock and opens a new CHANGELOG.md section
# fill in CHANGELOG.md, then commit
git tag v0.2.0 && git push origin main v0.2.0
```

A `v*` tag runs the **Release** workflow (`.github/workflows/release.yml`). It checks that the tag matches `VERSION`,
then builds `acam` for Linux, the signed release APK and the OBS plugin, and publishes them to GitHub Releases with
notes from `CHANGELOG.md` and a `SHA256SUMS` file. The **CI** workflow runs tests and builds on every push to `main`
and on every pull request.

The APK is signed with the release keystore from the GitHub Secrets `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`,
`ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD`. For local release builds, `android/keystore.properties` (not committed)
holds `storeFile`, `storePassword`, `keyAlias` and `keyPassword`. Every release must use the same keystore, because
Android refuses updates signed with a different key.

## Testing on the emulator

Emulator ports are not exposed to the host. Redirect through the emulator console (not ADB):

```bash
telnet localhost 5554     # type: auth <contents of ~/.emulator_console_auth_token>, then: redir add tcp:5000:5000
acam --headless --host 127.0.0.1 --token <token>
```

## License

[MIT](LICENSE) © 2026 Rizal Abdul Basith
