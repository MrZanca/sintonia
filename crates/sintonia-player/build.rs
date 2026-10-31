//! Trova libmpv e la collega.
//!
//! Windows: la cartella di libmpv (`mpv.lib`, `libmpv-2.dll`) si indica con `SINTONIA_MPV_DIR`;
//! se manca si usa `vendor/mpv` nella radice del workspace. La build deve essere **LGPL**.
//! La DLL viene copiata in `target/<profilo>` e in `target/<profilo>/deps`, così `cargo run` e
//! `cargo test` la trovano senza toccare il PATH.
//! Android: `SINTONIA_MPV_DIR` (o `vendor/mpv-android`) contiene una cartella per ABI
//! (`arm64-v8a/`, `x86_64/`) con `libmpv.so`, le librerie di FFmpeg e `libc++_shared.so`, come le
//! produce `scripts/libmpv-android.sh`. Le `.so` vanno nell'APK (`lib/<abi>/`): la cartella arriva
//! ai crate che dipendono da questo come `DEP_MPV_LIB_DIR`.
//! Linux e macOS: libmpv di sistema.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=SINTONIA_MPV_DIR");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "windows" {
        let dir = mpv_dir("mpv");
        let lib = dir.join("mpv.lib");
        let dll = dir.join("libmpv-2.dll");
        if !lib.is_file() || !dll.is_file() {
            panic!(
                "libmpv non trovata in {}: servono mpv.lib e libmpv-2.dll (vedi docs/sviluppo.md)",
                dir.display()
            );
        }
        println!("cargo:rerun-if-changed={}", lib.display());
        println!("cargo:rerun-if-changed={}", dll.display());
        println!("cargo:rustc-link-search=native={}", dir.display());
        copy_dll_next_to_binaries(&dll);
    } else if target_os == "android" {
        let abi = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("aarch64") => "arm64-v8a",
            Ok("x86_64") => "x86_64",
            other => panic!("ABI Android non prevista: {other:?} (ci sono arm64-v8a e x86_64)"),
        };
        let dir = mpv_dir("mpv-android").join(abi);
        let so = dir.join("libmpv.so");
        if !so.is_file() || !dir.join("libavcodec.so").is_file() {
            panic!(
                "libmpv per Android non trovata in {}: servono libmpv.so e le librerie di \
                 FFmpeg (vedi docs/sviluppo.md)",
                dir.display()
            );
        }
        println!("cargo:rerun-if-changed={}", so.display());
        println!(
            "cargo:rerun-if-changed={}",
            dir.join("libavcodec.so").display()
        );
        println!("cargo:rustc-link-search=native={}", dir.display());
        // av_jni_set_java_vm sta in libavcodec: senza la JavaVM mediacodec e audiotrack non partono
        println!("cargo:rustc-link-lib=dylib=avcodec");
        println!("cargo:lib_dir={}", dir.display());
    }
    println!("cargo:rustc-link-lib=dylib=mpv");
}

/// `SINTONIA_MPV_DIR`, oppure `vendor/<name>` nella radice del workspace.
fn mpv_dir(name: &str) -> PathBuf {
    match env::var_os("SINTONIA_MPV_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let manifest = PathBuf::from(
                env::var_os("CARGO_MANIFEST_DIR").expect("cargo imposta sempre CARGO_MANIFEST_DIR"),
            );
            manifest.join("..").join("..").join("vendor").join(name)
        }
    }
}

fn copy_dll_next_to_binaries(dll: &Path) {
    // OUT_DIR = target/<profilo>/build/sintonia-player-<hash>/out
    let Some(profile_dir) = env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .and_then(|out| out.ancestors().nth(3).map(Path::to_path_buf))
    else {
        println!("cargo:warning=impossibile ricavare la cartella del profilo da OUT_DIR");
        return;
    };
    let Some(name) = dll.file_name() else { return };
    for dir in [profile_dir.clone(), profile_dir.join("deps")] {
        let target = dir.join(name);
        let up_to_date = match (dll.metadata(), target.metadata()) {
            (Ok(src), Ok(dst)) => {
                src.len() == dst.len() && src.modified().ok() <= dst.modified().ok()
            }
            _ => false,
        };
        if !up_to_date {
            if let Err(err) =
                std::fs::create_dir_all(&dir).and_then(|()| std::fs::copy(dll, &target))
            {
                println!(
                    "cargo:warning=copia di {} non riuscita: {err}",
                    dll.display()
                );
            }
        }
    }
}
