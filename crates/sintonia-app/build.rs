//! Compila l'interfaccia con le traduzioni incluse.
//! (La DLL di libmpv la mette accanto all'eseguibile il build script di `sintonia-player`.)
//!
//! Traduzioni: `lang/<lingua>/LC_MESSAGES/sintonia-app.po`, stringhe originali in inglese,
//! senza contesto (così il file `.po` non dipende dai nomi dei componenti).

fn main() {
    println!("cargo:rerun-if-changed=lang");
    println!("cargo:rerun-if-changed=sintonia.rc");
    println!("cargo:rerun-if-changed=../../assets/logo/sintonia.ico");
    // icona dell'eseguibile su Windows (nessun effetto sugli altri sistemi)
    embed_resource::compile("sintonia.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("icona dell'eseguibile non compilata");
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("compilazione di ui/app.slint fallita");
}
