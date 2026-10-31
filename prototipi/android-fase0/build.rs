//! Compila l'interfaccia e collega la libmpv per Android indicata da `SINTONIA_MPV_DIR`
//! (cartella con `libmpv.so` e le librerie di FFmpeg per l'ABI in costruzione).

use std::env;
use std::path::PathBuf;

fn main() {
    slint_build::compile("ui/prova.slint").expect("compilazione di prova.slint");
    println!("cargo:rerun-if-env-changed=SINTONIA_MPV_DIR");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        let abi = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("aarch64") => "arm64-v8a",
            Ok("x86_64") => "x86_64",
            other => panic!("ABI Android non prevista: {other:?}"),
        };
        let base = env::var_os("SINTONIA_MPV_DIR").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default())
                .join("../../vendor/mpv-android")
        });
        let dir = base.join(abi);
        assert!(
            dir.join("libmpv.so").is_file(),
            "libmpv.so non trovata in {} (vedi docs/sviluppo.md)",
            dir.display()
        );
        println!("cargo:rustc-link-search=native={}", dir.display());
        // av_jni_set_java_vm vive in libavcodec: senza di essa mediacodec non parte
        println!("cargo:rustc-link-lib=dylib=avcodec");
    }
}
