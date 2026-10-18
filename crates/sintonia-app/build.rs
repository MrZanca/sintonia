//! Compila l'interfaccia con le traduzioni incluse.
//! (La DLL di libmpv la mette accanto all'eseguibile il build script di `sintonia-player`.)
//!
//! Traduzioni: `lang/<lingua>/LC_MESSAGES/sintonia-app.po`, stringhe originali in inglese,
//! senza contesto (così il file `.po` non dipende dai nomi dei componenti).

fn main() {
    println!("cargo:rerun-if-changed=lang");
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("compilazione di ui/app.slint fallita");
}
