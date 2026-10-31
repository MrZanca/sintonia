//! Compila l'interfaccia. libmpv e libavcodec li collega `sintonia-player` (il suo `build.rs` cerca
//! `vendor/mpv-android/<abi>` o `SINTONIA_MPV_DIR`).

fn main() {
    slint_build::compile("ui/prova.slint").expect("compilazione di prova.slint");
}
