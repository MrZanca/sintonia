//! Compila l'interfaccia. (La DLL di libmpv la mette accanto all'eseguibile `sintonia-player`.)

fn main() {
    slint_build::compile("ui/app.slint").expect("compilazione di ui/app.slint fallita");
}
