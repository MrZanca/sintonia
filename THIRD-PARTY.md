# Componenti di terze parti

## libmpv (LGPL-2.1-or-later) e FFmpeg (LGPL-3.0-or-later)

Sintonia usa libmpv come libreria dinamica separata (`libmpv-2.dll`), che si può sostituire con
un'altra build compatibile. La build distribuita è **LGPL**: mpv compilato con `-Dgpl=false` e
FFmpeg senza componenti GPL.

- Build: [zhongfly/mpv-winbuild](https://github.com/zhongfly/mpv-winbuild), release `2026-10-17-6f605372b9`
- Sorgenti di mpv: <https://github.com/mpv-player/mpv/commit/6f605372b9c0c397d60d3464e6fcb12e71f95d9e>
- Script e patch di compilazione: <https://github.com/zhongfly/mpv-winbuild> (`compile-lgpl-libmpv.patch`)

Il testo completo delle licenze va incluso nell'installer (M4).

## libmpv per Android (LGPL, M5 fase 2)

Compilata dal workflow `.github/workflows/libmpv-android.yml` (`scripts/libmpv-android.sh`) con le
`buildscripts` di [mpv-android](https://github.com/mpv-android/mpv-android) (MIT) al commit
`255901dd586fa750000fa0075f69d6ffdd26cd9c`, cambiando solo la licenza: mpv con `-Dgpl=false`, FFmpeg
senza `--enable-gpl` (resta `--enable-version3`, quindi **LGPL-3.0-or-later**: mbedtls è Apache-2.0).
Lo script si ferma se FFmpeg non risulta «LGPL version 3 or later» o se mpv non ha `HAVE_GPL 0`; niente
x264, x265 né altre parti GPL. Come per Windows, `libmpv.so` e le librerie di FFmpeg restano dinamiche
e sostituibili. Licenze complete in `LICENSES/` dentro lo zip della build.

| Componente | Versione | Licenza | Come entra |
|---|---|---|---|
| mpv | commit `6f605372b9` (come Windows) | LGPL-2.1-or-later | `libmpv.so` |
| FFmpeg | n9.0.2 | LGPL-3.0-or-later | `libav*.so`, `libsw*.so` |
| libplacebo | v7.360.1 | LGPL-2.1-or-later | statica in `libmpv.so` |
| libass | 0.17.5 | ISC | statica in `libmpv.so` |
| dav1d | 1.5.4 | BSD-2-Clause | statica in `libavcodec.so` |
| mbedtls | 3.6.7 | Apache-2.0 | statica (TLS di FFmpeg e di curl) |
| curl | 8.23.0 | curl (MIT-like) | statica in `libmpv.so` |
| libxml2 | 2.15.4 | MIT | statica |
| FreeType | 2.15.0 | FTL | statica |
| fontconfig | 2.18.3 | MIT-like | statica |
| FriBidi | 1.0.17 | LGPL-2.1-or-later | statica |
| HarfBuzz | 15.0.0 | MIT («Old MIT») | statica |
| libunibreak | 8.0 | zlib | statica |
| Lua | 5.2.4 | MIT | statica |
| libc++ dell'NDK r30 | — | Apache-2.0 with LLVM Exception | `libc++_shared.so` |

Tutte compatibili con la GPL-3.0 di Sintonia e con l'LGPL di libmpv (Apache-2.0 solo con le versioni
3 di GPL e LGPL, ed è il caso). Le librerie LGPL linkate statiche dentro `libmpv.so` (libplacebo,
FriBidi) sono a loro volta dentro una libreria LGPL sostituibile.

La build **GPL** usata nella fase 0 ([jarnedemeulemeester/libmpv-android](https://github.com/jarnedemeulemeester/libmpv-android)
v1.0.0, FFmpeg `--enable-gpl`) non si usa più e **non va distribuita**.

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
L'app su Android (fase 3) usa anche `android-activity` e `android_logger` (MIT o Apache-2.0); il suo
APK contiene solo codice Java di Sintonia (`SintoniaActivity`, GPL-3.0) e l'aiuto Java del backend
Android di Slint (con la licenza di Slint, vedi sopra).
Un riepilogo automatico completo (`cargo about` o simili) arriva con l'installer (M4).
