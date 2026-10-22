# ACAM: kamera Android sebagai webcam Linux lewat Wi-Fi atau USB

- `android/`: aplikasi Kotlin. Membuka server TCP di port 5000 dan menerima koneksi USB (Android Open Accessory), encode kamera ke H.264, dan mem-broadcast diri di UDP 5001.
- `pc/`: aplikasi Rust `acam` dengan GUI (gpui): preview kamera, panel koneksi Wi-Fi/USB, kontrol kamera. Decode H.264 (openh264) lalu tulis ke v4l2loopback. Mode CLI tersedia lewat `--headless`.
- UI Android memakai Jetpack Compose (tanpa preview kamera; preview ada di PC).
- `obs-plugin/`: plugin OBS "ACAM Camera" (C) yang mengambil video dari `acam` tanpa v4l2loopback.
- `PROTOCOL.md`: spesifikasi protokol. `CHANGELOG.md`: riwayat rilis. `TODO.md`: rencana (Windows, macOS).

Saat ini PC yang didukung hanya Linux x86_64; dukungan Windows dan macOS ada di [TODO.md](TODO.md).

## Instal dari release

Unduh dari [Releases](https://github.com/rjial/android-camera/releases/latest):

- **HP:** `acam-<versi>.apk`, lalu install (izinkan "install dari sumber tidak dikenal"). Kalau sebelumnya terpasang
  versi hasil build sendiri (debug), uninstall dulu, karena tanda tangannya berbeda.
- **PC:** `acam-<versi>-linux-x86_64.tar.gz`, lalu ekstrak dan jalankan `./acam`. Butuh Vulkan dan X11/Wayland.
- **OBS (opsional):** `acam-obs-plugin-<versi>-linux-x86_64.tar.gz`. Tutup OBS dulu, lalu:
  ```bash
  tar -xzf acam-obs-plugin-*-linux-x86_64.tar.gz -C ~/.config/obs-studio/plugins/
  ```

Cek file dengan `sha256sum -c SHA256SUMS`.

## Build

Aplikasi Android: buka folder `android/` di Android Studio, lalu Run. Bisa juga lewat command line kalau Gradle wrapper sudah ada:

```bash
./gradlew installDebug
```

Aplikasi PC:

```bash
cd pc && cargo build --release
```

Catatan dependensi: `gpui 0.2.2` dipasangkan dengan `gpui-component =0.5.1` (versi 0.6+ memakai fork gpui lain), dan
`libc` dikunci ke 0.2.188 di `Cargo.lock` karena 0.2.190+ menghapus `ENOATTR` yang dipakai dependensi gpui (`xattr`).
Jangan jalankan `cargo update` tanpa `cargo update -p libc --precise 0.2.188` sesudahnya.
GUI butuh Vulkan (driver Mesa/NVIDIA) dan X11 atau Wayland.

Plugin OBS (butuh `obs-devel`/header libobs dan `cmake`). **Tutup OBS dulu**, karena menimpa plugin yang sedang dimuat membuat OBS crash:

```bash
cd obs-plugin && cmake -B build -DCMAKE_BUILD_TYPE=Release && cmake --build build && cmake --install build
```

Plugin ter-install ke `~/.config/obs-studio/plugins/acam-source/` (`bin/64bit/acam-source.so` dan terjemahan di `data/locale/`).

## Pakai

1. Muat modul v4l2loopback (sekali per boot):
   ```bash
   sudo modprobe v4l2loopback devices=1 video_nr=10 card_label="Android Cam" exclusive_caps=1
   ```
2. Di HP, buka ACAM lalu tekan **Start**. Catat IP dan token yang tampil.
3. Di PC, buka GUI: `pc/target/release/acam`. Pilih Wi-Fi (klik HP di daftar "HP ditemukan", isi token) atau USB,
   lalu **Sambungkan**. Preview tampil di jendela; resolusi, fps, bitrate, ganti kamera, dan output webcam diatur dari panel kanan.

   Argumen koneksi langsung menyambung saat GUI terbuka, mis. `acam --usb`. Tanpa jendela (CLI):
   ```bash
   pc/target/release/acam --headless --host 192.168.1.23 --token 123456
   pc/target/release/acam --headless --discover --token 123456
   pc/target/release/acam --headless --usb
   ```
4. Pilih kamera **Android Cam** di browser, OBS, atau Zoom. Untuk cek cepat: `ffplay /dev/video10`.

### Lewat kabel USB (tanpa ADB, tanpa jaringan)

Pilih tab USB di GUI, atau `acam --headless --usb`.

`acam` mengalihkan HP ke mode Android Open Accessory. Pertama kali, HP menampilkan dialog
"Buka ACAM untuk ACAM Webcam?"; centang "selalu" lalu OK. Aplikasi terbuka dan streaming mulai
otomatis, tanpa token. Mode USB HP (MTP/ADB) kembali normal setelah kabel dicabut.

Butuh akses ke perangkat USB HP tanpa root. Paket `android-udev-rules` sudah mencakup sebagian besar merek.
Kalau muncul error izin, tambahkan aturan udev untuk vendor HP-mu dan untuk `18d1:2d00`/`18d1:2d01`.
Merek yang tidak dikenali bisa dipaksa dengan `--usb-vid <hex>`.

Webcam virtual dibuka sekali dengan ukuran koneksi pertama dan tetap terbuka saat reconnect; stream resolusi lain
di-scale ke ukuran itu, sehingga aplikasi seperti Zoom tidak kehilangan kameranya.

### Plugin OBS (tanpa v4l2loopback)

Di OBS: **Sources → + → ACAM Camera**. Source ini mengambil video dari `acam` (GUI atau `--headless`) yang sedang
berjalan lewat unix socket lokal, tanpa modul `v4l2loopback`, jadi `--no-webcam` atau switch webcam virtual yang mati tetap jalan.
Gambar mengikuti resolusi stream, dan OBS tersambung lagi otomatis saat acam dibuka ulang.
Bisa dipakai lebih dari satu source sekaligus. Kontrol kamera tetap diatur dari acam.

Latensi tambahan socket diukur ~1 ms rata-rata di 1080p30 (dicatat di log OBS tiap menit, baris `[acam]`).
Berbagi ke OBS bisa dimatikan lewat switch **Bagikan ke OBS** di GUI atau `--no-obs`. Path socket default
`$XDG_RUNTIME_DIR/acam/video.sock`, bisa diganti dengan variabel `ACAM_SOCKET` (berlaku untuk acam dan plugin) atau di properti source.

### Gambar NO SIGNAL

Saat tidak ada video, OBS dan webcam virtual menampilkan color bar ala TV dengan tulisan **NO SIGNAL** dan penyebabnya:

| keterangan | kapan |
|---|---|
| SINYAL HILANG | HP masih tersambung tapi tidak ada frame selama 2,5 detik (mis. Wi-Fi macet) |
| HP TERPUTUS | kabel dicabut, aplikasi HP ditutup, sedang menyambung ulang, atau GUI di-disconnect |
| BERBAGI KE OBS DIMATIKAN | switch **Bagikan ke OBS** mati atau `--no-obs` (hanya di OBS) |
| ACAM TIDAK BERJALAN | aplikasi acam tidak jalan (hanya di OBS, digambar oleh plugin) |

Batas 2,5 detik dipilih di atas shutter maksimal 1 detik, supaya shutter lambat tidak memicu NO SIGNAL. Koneksi baru dinyatakan
putus setelah 6 detik tanpa data, jadi saat aplikasi HP ditutup urutannya SINYAL HILANG lalu HP TERPUTUS.
Di OBS gambar ini bisa dimatikan lewat properti source (**Tampilkan gambar NO SIGNAL…**); source lalu kosong seperti dulu.

### Kontrol kamera

Bagian **GAMBAR** di panel GUI berisi exposure (auto + EV + AE lock, atau ISO + shutter manual), fokus
(auto, manual, fokus ulang), white balance (preset, Kelvin, AWB lock), dan zoom. Yang tampil hanya fitur yang
didukung kamera aktif di HP yang tersambung, misalnya kamera depan biasanya tanpa ISO/fokus manual. Pengaturan
dipertahankan saat reconnect dan kembali otomatis saat ganti kamera. Shutter bisa sampai batas sensor
(maks. 1 detik). Shutter lebih lama dari satu frame menurunkan fps stream (mis. 1/25 di 30 fps → 25 fps);
untuk 1/24 atau 1/25 tanpa itu, pilih fps 24/25 sekalian.

Perintah mode `--headless` (ketik lalu Enter):

| perintah | fungsi |
|---|---|
| `s`, `k`, `b 2000` | ganti kamera, minta keyframe, bitrate 2000 kbps |
| `caps`, `status` | kemampuan kamera aktif, nilai ISO/shutter/fokus saat ini |
| `iso 800`, `iso auto` | ISO manual / kembali auto |
| `shutter 1/250`, `shutter 20ms`, `shutter auto` | shutter manual |
| `ev -1.5` | kompensasi exposure |
| `focus 30` (cm), `focus inf`, `focus auto`, `refocus` | fokus |
| `wb daylight`, `wb 3200K`, `wb auto` | white balance (cloudy, shade, incandescent, fluorescent, warm-fluorescent, twilight) |
| `zoom 2.5` | zoom |
| `lock ae on`, `lock awb off` | kunci exposure / white balance |
| `reset` | semua kembali otomatis |

Opsi lain: `--size 1920x1080`, `--fps 30`, `--bitrate 6000`, `--front`, `--dump file.h264`, `--no-webcam`, `--no-obs`, `--lang en|id`.
Lihat `acam --help`.

## Bahasa

Aplikasi PC, aplikasi Android, dan plugin OBS tersedia dalam **English** dan **Bahasa Indonesia**. English menjadi
cadangan untuk teks yang belum diterjemahkan.

- **acam (PC):** pilih di bagian **BAHASA / LANGUAGE** di panel GUI (berlaku langsung dan disimpan di
  `~/.config/acam/language`), atau pakai `--lang id` / `ACAM_LANG=id`. Tanpa itu acam mengikuti bahasa sistem
  (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`). Urutan prioritas: `--lang` → `ACAM_LANG` → pilihan GUI → bahasa sistem → English.
- **Aplikasi Android:** mengikuti bahasa HP. Di Android 13+ bisa diganti per aplikasi: Setelan → Aplikasi → ACAM → Bahasa.
- **Plugin OBS:** mengikuti bahasa antarmuka OBS (Settings → General → Language). Log OBS selalu dalam English.

Keterangan di gambar NO SIGNAL memakai bahasa masing-masing output: webcam virtual mengikuti bahasa acam, sedangkan
source OBS mengikuti bahasa OBS.

### Menambah bahasa

Tiap bagian memakai format terjemahan standarnya. Cukup tambah file, tanpa mengubah kode:

| bagian | salin dari | jadi (contoh Jerman) |
|---|---|---|
| acam (PC) | `pc/locales/en.yml` | `pc/locales/de.yml` |
| Android | `android/app/src/main/res/values/strings.xml` | `android/app/src/main/res/values-de/strings.xml` |
| Plugin OBS | `obs-plugin/data/locale/en-US.ini` | `obs-plugin/data/locale/de-DE.ini` |

Aturannya:
- Terjemahkan nilainya saja. Nama key dan placeholder (`%{name}` di PC, `%1$s`/`%1$d` di Android) harus tetap.
- Di PC, `cargo test` memeriksa bahwa key file baru sama dengan `en.yml` dan placeholdernya utuh. Bahasa baru otomatis
  muncul di pemilih bahasa GUI setelah di-build ulang.
- Di Android, bahasa baru otomatis masuk ke pilihan bahasa per aplikasi (`generateLocaleConfig`). `./gradlew lintDebug` memberi tahu string yang belum diterjemahkan.
- Teks `nosignal.*` / `NoSignal.*` digambar dengan font bawaan A–Z, jadi hanya huruf kapital A–Z dan spasi yang tampil.
  Aksen Latin (é, ñ, ü, …) otomatis dibuang; huruf non-Latin (Cyrillic, CJK, Arab, …) belum bisa tampil di gambar itu.

## Versi dan rilis

Proyek ini memakai [Semantic Versioning](https://semver.org/lang/id/). Satu versi di file `VERSION` berlaku untuk
aplikasi PC, aplikasi Android (`versionName`; `versionCode` = major·10000 + minor·100 + patch), dan plugin OBS.
Naikkan **patch** untuk perbaikan, **minor** untuk fitur baru, dan **major** untuk perubahan protokol yang tidak kompatibel
(selama `0.x`, perubahan protokol boleh terjadi di minor).

Merilis versi baru:

```bash
scripts/version.sh bump 0.2.0      # ubah VERSION, pc/Cargo.toml, Cargo.lock, dan buka bagian baru di CHANGELOG.md
# isi CHANGELOG.md, lalu commit
git tag v0.2.0 && git push origin main v0.2.0
```

Tag `v*` menjalankan workflow **Release** (`.github/workflows/release.yml`). Workflow ini memeriksa tag cocok dengan
`VERSION`, lalu mem-build `acam` Linux, APK release yang ditandatangani, dan plugin OBS. Hasilnya diterbitkan ke GitHub
Releases beserta catatan dari `CHANGELOG.md` dan `SHA256SUMS`. Workflow **CI** menjalankan test dan build di setiap push
ke `main` dan setiap pull request.

APK ditandatangani dengan keystore release dari GitHub Secrets `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`,
`ANDROID_KEY_ALIAS`, dan `ANDROID_KEY_PASSWORD`. Untuk build release lokal, `android/keystore.properties` (tidak di-commit)
berisi `storeFile`, `storePassword`, `keyAlias`, dan `keyPassword`. Keystore yang sama harus dipakai untuk semua rilis,
karena Android menolak update dengan tanda tangan berbeda.

## Uji di emulator

Port emulator tidak terbuka ke host. Redirect lewat konsol emulator (bukan ADB):

```bash
telnet localhost 5554     # ketik: auth <isi ~/.emulator_console_auth_token>, lalu: redir add tcp:5000:5000
acam --headless --host 127.0.0.1 --token <token>
```
