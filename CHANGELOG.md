# Catatan rilis NovaWrite

Satu bagian per versi. Saat membuat tag `vX.Y.Z`, workflow memakai bagian `## vX.Y.Z` sebagai isi release,
dan isi itu juga yang tampil di jendela "Update tersedia" di app.
Kalau bagian itu tidak ada, dipakai bagian `## Belum dirilis`. Ganti judulnya dengan nomor tag sebelum rilis.

## Belum dirilis

### Ditambahkan
- Pemberitahuan update: saat app dibuka dan ada versi baru di GitHub, muncul jendela berisi catatan perubahan dan tombol Unduh.
- Ikon NovaWrite di layar utama HP (sebelumnya ikon robot bawaan Android).

### Diubah
- Tautan repo di app dan metadata dipindah ke github.com/nightbest001/novawrite-android.
- APK sekarang selalu dibangun dalam mode release: jauh lebih kecil daripada APK debug sebelumnya (242 MB) dan lebih cepat dijalankan.
- Versi dan kode versi APK diambil dari nama tag release.

### Diperbaiki
- APK di release sebelumnya adalah file sementara yang belum ditandatangani sehingga tidak bisa dipasang. Sekarang yang diunggah adalah APK akhir yang sudah ditandatangani, dan build gagal kalau tanda tangannya tidak valid.
- Build GitHub Actions tidak lagi gagal saat memilih target contoh PC.

### Dihapus
- Tidak ada.
