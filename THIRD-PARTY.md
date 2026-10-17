# Componenti di terze parti

## libmpv (LGPL-2.1-or-later) e FFmpeg (LGPL-3.0-or-later)

Sintonia usa libmpv come libreria dinamica separata (`libmpv-2.dll`), che si può sostituire con
un'altra build compatibile. La build distribuita è **LGPL**: mpv compilato con `-Dgpl=false` e
FFmpeg senza componenti GPL.

- Build: [zhongfly/mpv-winbuild](https://github.com/zhongfly/mpv-winbuild), release `2026-10-17-6f605372b9`
- Sorgenti di mpv: <https://github.com/mpv-player/mpv/commit/6f605372b9c0c397d60d3464e6fcb12e71f95d9e>
- Script e patch di compilazione: <https://github.com/zhongfly/mpv-winbuild> (`compile-lgpl-libmpv.patch`)

Il testo completo delle licenze va incluso nell'installer (M4).

## Slint

Slint è usato con la licenza **GPLv3**, compatibile con la licenza di Sintonia.
Per un eventuale modulo Premium chiuso servirà la licenza *Royalty-free* di Slint, che copre le
app desktop proprietarie: va riverificata prima di M4.

## Crate Rust

Le dipendenze Rust e le loro licenze sono elencate in `Cargo.lock`. Un riepilogo automatico
(`cargo about` o simili) arriva con l'installer (M4).
