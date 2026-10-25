//! L'interfaccia in proporzione alla finestra.
//!
//! Le pagine sono disegnate per una finestra di 1320 × 800 punti. A finestra ingrandita o a schermo
//! intero, invece di lasciare tutto piccolo in un angolo con tanto spazio vuoto, si ingrandisce
//! tutta l'interfaccia insieme (come su un televisore): Slint lo fa da sé se gli si dice un fattore
//! di scala più grande di quello di Windows. A finestra normale resta quello di Windows, così
//! trascinando i bordi non cambia niente.

use slint::platform::WindowEvent;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, LogicalSize};

use crate::MainWindow;

/// La grandezza per cui sono disegnate le pagine (punti logici).
const DESIGN_WIDTH: f64 = 1320.0;
const DESIGN_HEIGHT: f64 = 800.0;
/// Oltre questo ingrandimento le pagine non guadagnano niente.
const MAX_ZOOM: f64 = 3.0;

/// Quanto ingrandire per una finestra di `width` × `height` punti (al fattore di Windows): quanto
/// basta perché le pagine riempiano il lato più corto, mai meno di 1.
pub fn zoom_for(width: f64, height: f64) -> f64 {
    let zoom = (width / DESIGN_WIDTH).min(height / DESIGN_HEIGHT);
    if zoom.is_finite() {
        zoom.clamp(1.0, MAX_ZOOM)
    } else {
        1.0
    }
}

/// Ricalcola il fattore di scala dopo un cambio di grandezza o di stato della finestra.
pub fn apply(main: &MainWindow) {
    let window = main.window();
    let Some((physical, os_scale, large)) = window.with_winit_window(|w| {
        let large = w.is_maximized() || w.fullscreen().is_some();
        (w.inner_size(), w.scale_factor(), large)
    }) else {
        return;
    };
    if physical.width == 0 || physical.height == 0 || os_scale <= 0.0 {
        return;
    }
    let zoom = if large {
        zoom_for(
            f64::from(physical.width) / os_scale,
            f64::from(physical.height) / os_scale,
        )
    } else {
        1.0
    };
    let scale = (os_scale * zoom) as f32;
    if (window.scale_factor() - scale).abs() < 0.01 {
        return;
    }
    log::debug!("interfaccia ingrandita {zoom:.2} volte");
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: scale,
    });
    // le pagine si ridispongono sulla nuova grandezza logica della finestra
    window.dispatch_event(WindowEvent::Resized {
        size: LogicalSize::new(
            physical.width as f32 / scale,
            physical.height as f32 / scale,
        ),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_follows_the_shorter_side() {
        assert_eq!(zoom_for(1320.0, 800.0), 1.0);
        // più piccola di così non si rimpicciolisce
        assert_eq!(zoom_for(1100.0, 700.0), 1.0);
        // 2560 × 1369 (finestra ingrandita su un 1440p): comanda l'altezza
        assert!((zoom_for(2560.0, 1369.0) - 1369.0 / 800.0).abs() < 1e-9);
        // schermo molto largo: comanda l'altezza, schermo molto alto la larghezza
        assert!((zoom_for(3840.0, 1080.0) - 1.35).abs() < 1e-9);
        assert!((zoom_for(1584.0, 2000.0) - 1.2).abs() < 1e-9);
        assert_eq!(zoom_for(10000.0, 10000.0), MAX_ZOOM);
        assert_eq!(zoom_for(f64::NAN, 800.0), 1.0);
    }
}
