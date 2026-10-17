//! Trova libmpv e la collega.
//!
//! Windows: la cartella di libmpv (`mpv.lib`, `libmpv-2.dll`) si indica con `SINTONIA_MPV_DIR`;
//! se manca si usa `vendor/mpv` nella radice del workspace. La build deve essere **LGPL**.
//! La DLL viene copiata in `target/<profilo>` e in `target/<profilo>/deps`, così `cargo run` e
//! `cargo test` la trovano senza toccare il PATH.
//! Linux e macOS: libmpv di sistema (arriva con M5).

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=SINTONIA_MPV_DIR");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "windows" {
        let dir = match env::var_os("SINTONIA_MPV_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => {
                let manifest = PathBuf::from(
                    env::var_os("CARGO_MANIFEST_DIR")
                        .expect("cargo imposta sempre CARGO_MANIFEST_DIR"),
                );
                manifest.join("..").join("..").join("vendor").join("mpv")
            }
        };
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
    }
    println!("cargo:rustc-link-lib=dylib=mpv");
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
