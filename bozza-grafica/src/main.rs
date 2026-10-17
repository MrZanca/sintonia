//! Bozza grafica di Sintonia: finestra principale + mini player separato.
//!
//! Il mini player (`PipWindow`) è una finestra a parte, senza cornice e sempre in primo piano,
//! quindi resta visibile sopra le altre applicazioni anche se la finestra principale è ridotta a icona.
//! Nella versione vera i modelli arriveranno da `sintonia-core` e il riquadro video ospiterà libmpv.
//!
//! Nota: questo file non è stato compilato (su questo PC Rust non è installato); la bozza è stata
//! provata con lo stesso `ui/app.slint` caricato da Python.

// Il gestore di finestre di Windows non apre una console in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use slint::{ComponentHandle, Model, PhysicalPosition};

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let main = MainWindow::new()?;
    let pip = PipWindow::new()?;

    // apre il mini player per il canale scelto, in basso a destra rispetto alla finestra principale
    {
        let pip_weak = pip.as_weak();
        let main_weak = main.as_weak();
        main.on_request_pip(move |ch| {
            let (Some(pip), Some(main)) = (pip_weak.upgrade(), main_weak.upgrade()) else {
                return;
            };
            pip.set_num(ch.num);
            pip.set_name(ch.name);
            pip.set_now(ch.now);
            pip.set_tint(ch.tint);
            let _ = pip.show();
            let (pos, size) = (main.window().position(), main.window().size());
            let scale = pip.window().scale_factor();
            pip.window().set_position(PhysicalPosition::new(
                pos.x + size.width as i32 - (400.0 * scale) as i32,
                pos.y + size.height as i32 - (260.0 * scale) as i32,
            ));
        });
    }
    {
        let pip_weak = pip.as_weak();
        main.on_close_pip(move || {
            if let Some(pip) = pip_weak.upgrade() {
                let _ = pip.hide();
            }
        });
    }

    // trascinamento: la finestra senza cornice si sposta con il mouse
    {
        let pip_weak = pip.as_weak();
        pip.on_moved(move |dx, dy| {
            if let Some(pip) = pip_weak.upgrade() {
                let window = pip.window();
                let (scale, pos) = (window.scale_factor(), window.position());
                window.set_position(PhysicalPosition::new(
                    pos.x + (dx * scale) as i32,
                    pos.y + (dy * scale) as i32,
                ));
            }
        });
    }
    {
        let pip_weak = pip.as_weak();
        pip.on_dismiss(move || {
            if let Some(pip) = pip_weak.upgrade() {
                let _ = pip.hide();
            }
        });
    }

    // «espandi»: chiude il mini player e riprende il canale a tutto schermo nell'app
    {
        let pip_weak = pip.as_weak();
        let main_weak = main.as_weak();
        pip.on_expand(move || {
            let (Some(pip), Some(main)) = (pip_weak.upgrade(), main_weak.upgrade()) else {
                return;
            };
            let num = pip.get_num();
            if let Some(index) = main.get_channels().iter().position(|c| c.num == num) {
                main.set_selected(index as i32);
            }
            main.set_playing(true);
            let _ = pip.hide();
            let _ = main.show();
        });
    }

    main.run()
}
