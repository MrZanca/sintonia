//! Bozza da provare col mouse: sfondi delle sezioni, movimento dello sfondo e bordi animati.
//! Avvio: `cargo run --manifest-path bozza-animazioni/Cargo.toml`

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let bozza = Bozza::new()?;
    // per le prove: SINTONIA_BOZZA="sfondo,movimento,bordi" (numeri da 0 a 3)
    if let Ok(start) = std::env::var("SINTONIA_BOZZA") {
        let n: Vec<i32> = start.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        if let [bg, motion, border] = n[..] {
            bozza.set_bg_kind(bg);
            bozza.set_motion(motion);
            bozza.set_border(border);
        }
    }
    bozza.run()
}
