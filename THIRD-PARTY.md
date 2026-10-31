# Componenti di terze parti

## libmpv (LGPL-2.1-or-later) e FFmpeg (LGPL-3.0-or-later)

Sintonia usa libmpv come libreria dinamica separata (`libmpv-2.dll`), che si può sostituire con
un'altra build compatibile. La build distribuita è **LGPL**: mpv compilato con `-Dgpl=false` e
FFmpeg senza componenti GPL.

- Build: [zhongfly/mpv-winbuild](https://github.com/zhongfly/mpv-winbuild), release `2026-10-17-6f605372b9`
- Sorgenti di mpv: <https://github.com/mpv-player/mpv/commit/6f605372b9c0c397d60d3464e6fcb12e71f95d9e>
- Script e patch di compilazione: <https://github.com/zhongfly/mpv-winbuild> (`compile-lgpl-libmpv.patch`)

Il testo completo delle licenze va incluso nell'installer (M4).

## libmpv per Android (solo prove interne, M5 fase 0)

Per misurare la fattibilità su Android si è usata una build **GPL** (FFmpeg `--enable-gpl --enable-version3`):
[jarnedemeulemeester/libmpv-android](https://github.com/jarnedemeulemeester/libmpv-android) v1.0.0 (MIT per
gli script), derivata dalle `buildscripts` di [mpv-android](https://github.com/mpv-android/mpv-android).
**Non va distribuita**: per le Release serve una libmpv LGPL (mpv `-Dgpl=false`, FFmpeg senza
`--enable-gpl`), da compilare in M5 fase 2. Dipendenze dichiarate da quella build: dav1d (BSD-2),
mbedtls (Apache-2.0), libxml2 (MIT), libass (ISC), FreeType (FTL), HarfBuzz (MIT), FriBidi (LGPL-2.1),
Lua (MIT).

## Slint

Slint è usato con la licenza **GPLv3**, compatibile con la licenza di Sintonia.
Per un eventuale modulo Premium chiuso servirà la licenza *Royalty-free* di Slint, che copre le
app desktop proprietarie: va riverificata prima di M4.

## SQLite

Incluso nell'eseguibile (crate `rusqlite`, versione `bundled`): pubblico dominio.

## Crate Rust

Le dipendenze Rust sono elencate in `Cargo.lock`; le principali (tokio, reqwest, rustls, rusqlite,
chacha20poly1305, image, keyring) sono sotto licenze MIT, Apache-2.0, ISC o simili, compatibili con
la GPL-3.0. La crittografia di rustls usa aws-lc-rs (Apache-2.0 e ISC, con parti OpenSSL).
Solo su Android (M5 fase 1) il core usa `jni` (MIT o Apache-2.0) al posto di `keyring`; le
autorità di certificazione sono quelle di sistema di Android, lette dai file, senza codice Java
aggiunto all'APK.
Un riepilogo automatico completo (`cargo about` o simili) arriva con l'installer (M4).
