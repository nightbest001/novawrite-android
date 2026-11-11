# NovaWrite

Pengolah kata untuk **Android** dengan tab **MCP**, supaya AI bisa membuka, mengedit, dan menyimpan dokumen di aplikasi ini.

NovaWrite berbasis WordCraft, pengolah kata open source yang ditulis dengan Rust. Repo ini hanya untuk APK Android. Aplikasi PC, CLI, versi web, dan paket installer PC sudah dibuang.

> **Status:** tahap awal. Kode belum diuji di perangkat nyata. APK dibuat oleh GitHub Actions, dan build pertama bisa saja gagal. Kalau ketemu masalah, buka Issue dan sertakan log.

## Pasang APK

1. Buka halaman **Releases** repo ini.
2. Unduh `NovaWrite-<versi>-android.apk`.
3. Di HP, izinkan pemasangan dari sumber tidak dikenal, lalu buka file APK.
4. Buka **NovaWrite**.

Syarat: HP Android 64-bit (arm64).

APK dari release yang ditandatangani dengan keystore bisa diperbarui di atas versi lama. APK debug (tanpa keystore) memakai kunci debug, jadi hanya cocok untuk mencoba.

## Yang bisa dipakai

- Menulis dan menyunting dokumen dengan mesin WordCraft.
- Membuka dan menyimpan berbagai format: `docx`, `odt`, `rtf`, `txt`, `md`, `html`, dan lainnya.
- Ekspor ke PDF.
- Pemilih file bawaan aplikasi, karena HP tidak punya dialog file seperti PC.
- Pemulihan dokumen otomatis kalau aplikasi tertutup mendadak.
- Tab **MCP**: menghubungkan aplikasi ke server MCP supaya AI bisa mengendalikannya.

Fitur yang dulu hanya ada di PC, sekarang jalan di HP:

- **Clipboard sistem.** Salin dan tempel teks, teks berformat (HTML), dan gambar (screenshot atau "Salin gambar") antar aplikasi. Tombol salin URL MCP juga sampai ke clipboard sistem.
- **Cetak.** File ▸ Cetak membuat PDF, menyimpannya di `Download/NovaWrite`, lalu membukanya di aplikasi PDF pilihanmu. Cetak dari sana lewat layanan cetak Android. Perlu Android 10 ke atas.
- **Jadikan aplikasi default.** Tombol di File ▸ Opsi membuka Setelan ▸ Aplikasi default. NovaWrite juga muncul di "Buka dengan" untuk `docx`, `odt`, `rtf`, `txt`, dan `md`.
- **Buka dari aplikasi lain.** File yang dibuka lewat "Buka dengan NovaWrite" disalin ke `Documents/Incoming`, lalu dibuka.
- **Geser dengan jari.** Menggeser jari di halaman menggulir dokumen. Ketuk untuk menaruh kursor.
- **Keyboard layar** muncul saat mengetuk halaman atau kolom teks.
- **Kontrol lebih besar.** Tombol dan jarak antar kontrol diperbesar supaya muat jari.
- **MCP lebih awet.** Selama MCP **Online**, layar dijaga tetap menyala dan CPU tidak tidur.
- **Animasi** mengikuti setelan animasi sistem.

Pita (ribbon) tertutup saat pertama dibuka supaya halaman punya ruang. Ketuk tab untuk membukanya.

## Lokasi dokumen

Dokumen dan hasil simpan ada di:

```
Android/data/com.nightbest001.novawrite/files/Documents
```

Folder ini bisa dibuka lewat USB atau aplikasi file manager. Pengaturan dan kode MCP disimpan di folder privat aplikasi, tidak terlihat dari luar.

Sebagian file manager di Android 11 ke atas membatasi isi `Android/data`. Kalau tidak bisa dibuka, pakai USB atau file manager lain.

## Hubungkan AI lewat MCP

1. Buka tab **MCP**.
2. Isi **Server** dengan alamat server MCP kamu.
3. Ketuk **Generate MCP**. Kode baru dibuat dan URL MCP tersalin.
4. Tempel URL itu di pengaturan konektor AI.
5. Pastikan status berubah menjadi **Online**.

Hal yang perlu diingat:

- Kode adalah rahasia. Siapa pun yang punya URL MCP bisa mengendalikan aplikasimu. Jangan dibagikan.
- Bridge MCP hanya hidup **selama aplikasi tampil di layar**. Android menghentikannya saat aplikasi masuk latar belakang.
- Opsi **Allow file access** memberi AI izin membuka, menyimpan, dan mengekspor file di folder aplikasi. Matikan kalau tidak perlu.
- Ketuk **Disconnect** untuk memutus. **Generate MCP** lagi membuat kode baru dan mematikan kode lama.

## Belum ada

- Layanan latar belakang agar MCP tetap hidup saat aplikasi ditutup atau layar terkunci. Ini butuh kode Java (foreground service), dan `cargo apk` tidak bisa memuat Java.
- Pilihan "Buka dengan" saat aplikasi **sudah berjalan**. Android hanya menyerahkan file saat aplikasi baru dibuka. Kalau sudah terbuka, pakai File ▸ Buka.
- Input IME (komposisi huruf, misalnya Jepang/Mandarin atau saran kata). NativeActivity hanya menerima tombol biasa.
- Tata letak khusus layar sentuh. Pita masih bergaya PC, hanya dibuat lebih lega.
- Nama di dalam aplikasi (menu, dialog Tentang) sebagian masih tertulis "WordCraft".

## Build sendiri

Lewat GitHub Actions:

1. Buka tab **Actions**, pilih workflow **mobile**, lalu **Run workflow**.
2. Isi `tag`, misalnya `v0.1.0`, supaya hasilnya masuk ke Releases. Kosongkan kalau cukup artifact.
3. Opsional, untuk APK bertanda tangan: simpan secret `ANDROID_KEYSTORE_BASE64` dan `ANDROID_KEYSTORE_PASSWORD`.

Lewat terminal:

```bash
rustup target add aarch64-linux-android
cargo install cargo-apk
cd apps/wordcraft-mobile
cargo apk build
```

Detail Android ada di [docs/mobile.md](docs/mobile.md).

## Lisensi dan kredit

NovaWrite berbasis WordCraft karya ArtCraft Team dan kontributor WordCraft, berlisensi **MIT atau Apache-2.0**. Teks lisensi ada di `LICENSE-MIT` dan `LICENSE-APACHE`. Daftar komponen pihak ketiga ada di `NOTICE` dan `ATTRIBUTION.md`.

Repo ini bukan repo resmi WordCraft dan tidak berafiliasi dengan pembuatnya.
