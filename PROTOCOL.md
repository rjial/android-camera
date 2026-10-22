# ACAM v1

Streaming H.264 dari kamera Android ke PC lewat TCP. Semua angka big-endian.

- HP = **server** TCP, port default `5000`.
- PC = **klien**.
- Lewat USB, byte stream yang sama berjalan di atas endpoint bulk Android Open Accessory
  (identitas: manufacturer `ACAM`, model `ACAM Webcam`). Token diabaikan karena mencolok kabel
  sudah dianggap pairing.
- Discovery (opsional): HP mem-broadcast UDP ke port `5001` setiap 1 detik dengan payload
  `"ACAM" | ver u8 | tcp_port u16 | name_len u8 | name[name_len]` (UTF-8).

## Handshake

Klien → server (19 byte):

| field        | tipe  | keterangan                        |
|--------------|-------|-----------------------------------|
| magic        | 4B    | `"ACAM"`                          |
| version      | u8    | `1`                               |
| width        | u16   | resolusi yang diminta             |
| height       | u16   |                                   |
| fps          | u8    |                                   |
| bitrate_kbps | u32   |                                   |
| camera       | u8    | `0` = belakang, `1` = depan       |
| token        | u32   | token pairing yang tampil di HP   |

Server → klien (12 byte):

| field   | tipe | keterangan                                                    |
|---------|------|---------------------------------------------------------------|
| magic   | 4B   | `"ACAM"`                                                      |
| version | u8   | `1`                                                           |
| status  | u8   | `0` OK, `1` token salah, `2` versi tidak didukung, `3` kamera gagal |
| width   | u16  | resolusi yang benar-benar dipakai encoder                     |
| height  | u16  |                                                               |
| fps     | u8   |                                                               |
| codec   | u8   | `1` = H.264                                                   |

Kalau `status != 0`, server menutup koneksi.

## Paket

Setelah handshake, kedua arah memakai frame yang sama:

```
type u8 | flags u8 | len u32 | pts_us u64 | payload[len]
```

Header 14 byte. `len` maksimum 8 MiB.

| type   | arah    | payload / arti                                                          |
|--------|---------|-------------------------------------------------------------------------|
| `0x01` | HP → PC | `CODEC_CONFIG`: SPS+PPS Annex-B. Selalu dikirim sebelum frame pertama   |
| `0x02` | HP → PC | `FRAME`: satu access unit H.264 Annex-B. `flags & 1` = keyframe         |
| `0x10` | PC → HP | `REQ_KEYFRAME`: tanpa payload                                           |
| `0x11` | PC → HP | `SWITCH_CAMERA`: tanpa payload, ganti kamera depan/belakang             |
| `0x12` | PC → HP | `SET_BITRATE`: payload `u32` kbps                                       |
| `0x13` | PC → HP | `BYE`: PC menutup sesi; HP langsung melepas kamera                      |
| `0x14` | PC → HP | `SET_CONTROLS`: TLV kontrol kamera yang berubah (lihat di bawah)        |
| `0x22` | HP → PC | `CAPABILITIES`: TLV kemampuan kamera aktif; setelah CONFIG & ganti kamera |
| `0x23` | HP → PC | `CAMERA_STATE`: TLV nilai aktual (ISO, shutter, fokus, zoom), ≤ 4×/detik  |
| `0x20` | dua arah | `PING`: `pts_us` = waktu pengirim                                      |
| `0x21` | dua arah | `PONG`: `pts_us` disalin dari PING                                     |

Satu klien aktif. Kalau klien baru terhubung, klien lama diputus. Kalau tidak ada data
selama 6 detik, koneksi dianggap mati. Setelah `SWITCH_CAMERA`, server mengirim
`CODEC_CONFIG` baru dan resolusi bisa berubah, jadi klien membaca resolusi dari SPS/hasil decode,
bukan hanya dari handshake.

## Kontrol kamera (TLV)

Payload `SET_CONTROLS`, `CAPABILITIES`, dan `CAMERA_STATE` berisi pasangan berulang
`key u8 | value i64`. Key yang tidak dikenal diabaikan. Pasangan terakhir yang terpotong juga diabaikan.

Fitur bergantung pada HP dan kamera. HP hanya melaporkan apa yang benar-benar didukung kamera aktif;
key kapabilitas yang tidak ada berarti fitur itu tidak tersedia. HP tetap meng-clamp atau mengabaikan
nilai yang tidak didukung, jadi klien tidak bisa membuat kamera error.

### Key kontrol (`SET_CONTROLS`, juga dipakai `CAMERA_STATE`)

| key | nama | nilai |
|-----|------|-------|
| 1 | AE_MODE | 0 auto, 1 manual (ISO + shutter) |
| 2 | ISO | sensitivitas |
| 3 | EXPOSURE_NS | lama exposure (maks. 1 s); lebih lama dari 1 frame menurunkan fps stream |
| 4 | EV_STEPS | kompensasi exposure dalam step (lihat EV_STEP) |
| 5 | AE_LOCK | 0/1 |
| 6 | AF_MODE | 0 continuous, 1 manual |
| 7 | FOCUS_MILLI_DIOPTER | jarak fokus manual; 0 = ∞, 1000 = 1 m |
| 8 | AF_TRIGGER | 1 = fokus sekali lalu tahan |
| 9 | AWB_MODE | `CONTROL_AWB_MODE_*` Camera2 (1 auto, 2 lampu pijar, 3 neon, 4 neon hangat, 5 siang, 6 mendung, 7 senja, 8 teduh); 0 = Kelvin manual |
| 10 | WB_KELVIN | 2000–10000; perkiraan, relatif terhadap white balance otomatis |
| 11 | AWB_LOCK | 0/1 |
| 12 | ZOOM_MILLI | 1000 = 1× |
| 99 | ERROR | hanya di `CAMERA_STATE`: HP menolak kombinasi terakhir dan memakai pengaturan sebelumnya |

### Key kapabilitas (`CAPABILITIES`)

| key | arti |
|-----|------|
| 100 | ISO/shutter manual didukung (0/1) |
| 101 / 102 | ISO min / max |
| 103 / 104 | exposure min / max (ns) |
| 105 / 106 | EV min / max (step) |
| 107 | ukuran satu step EV ×1000 |
| 108 | fokus terdekat (milli-dioptri); 0 = tidak ada fokus manual |
| 109 | bitmask preset AWB (bit n = `CONTROL_AWB_MODE` n) |
| 110 | white balance Kelvin manual didukung (0/1) |
| 111 / 112 | zoom min / max ×1000 |
| 113 / 114 | AE lock / AWB lock tersedia (0/1) |
| 115 | durasi satu frame (ns) pada fps yang diminta |
| 116 | autofocus tersedia (0/1) |

## Berbagi ke OBS (`ACVF`)

Protokol lokal antara `acam` (server) dan plugin OBS "ACAM Camera" (klien), satu arah dari acam ke plugin.

- **Socket:** unix stream di `$ACAM_SOCKET`, kalau tidak ada `$XDG_RUNTIME_DIR/acam/video.sock`, kalau tidak ada
  `/tmp/acam-<uid>/video.sock`. Folder dibuat mode 0700 dan socket 0600, jadi hanya user yang sama yang bisa membaca.
- **Frame:** header 24 byte little-endian, lalu I420 rapat (`w·h` byte Y, lalu `w/2·h/2` byte U, lalu V).

| offset | ukuran | isi |
|--------|--------|-----|
| 0 | 4 | magic `ACVF` |
| 4 | 4 | width (u32) |
| 8 | 4 | height (u32) |
| 12 | 8 | `CLOCK_MONOTONIC` ns saat acam mengirim (u64), untuk mengukur latensi |
| 20 | 1 | color: 0 = BT.601, 1 = BT.709 |
| 21 | 1 | range: 0 = limited |
| 22 | 1 | khusus frame idle: alasan (0 = HP terputus, 1 = sinyal hilang, 2 = berbagi ke OBS dimatikan) |
| 23 | 1 | padding |

- **Idle:** `width = height = 0` tanpa payload berarti tidak ada video; byte 22 menyebut alasannya dan plugin menampilkan
  gambar NO SIGNAL. acam mengirimnya ulang tiap 500 ms selama tidak ada video, supaya source yang baru tersambung juga mendapatkannya.
- Klien lambat kehilangan frame (antrean 2 frame per klien), tidak pernah menahan pipeline acam.
- Klien memutus koneksi kalau magic salah atau ukuran > 8192 atau ganjil, lalu menyambung lagi.
