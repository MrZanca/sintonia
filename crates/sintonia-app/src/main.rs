//! Sintonia: player IPTV per PC.
//!
//! M0: la finestra della bozza grafica con il video di libmpv dentro (finestra principale e mini
//! player). I canali sono ancora quelli finti della bozza, collegati a flussi pubblici di prova
//! (vedi [`demo`]).

// Niente console in release su Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod demo;
mod video;

use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use sintonia_player::{Player, PlayerConfig, PlayerError};
use slint::{ComponentHandle, Model, PhysicalPosition};

use crate::video::VideoRouter;

/// Il codice generato da `ui/app.slint`.
mod ui {
    // codice generato da slint-build: i lint del workspace valgono per il nostro codice, non per questo
    #![allow(
        missing_debug_implementations,
        clippy::all,
        clippy::unwrap_used,
        clippy::undocumented_unsafe_blocks
    )]
    slint::include_modules!();
}
pub use ui::*;

/// Attesa prima di aprire un canale quando si scorre l'elenco.
const ZAP_DELAY: Duration = Duration::from_millis(350);

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error("interfaccia: {0}")]
    Platform(#[from] slint::PlatformError),
    #[error("ciclo degli eventi: {0}")]
    EventLoop(#[from] slint::EventLoopError),
    #[error("collegamento del video alla finestra: {0}")]
    Notifier(#[from] slint::SetRenderingNotifierError),
    #[error("{0}")]
    Player(#[from] PlayerError),
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,mpv=warn"))
        .init();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log::error!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), AppError> {
    // FemtoVG su OpenGL: serve un contesto OpenGL in cui far disegnare mpv.
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg".into())
        .require_opengl()
        .select()?;

    let (major, minor) = sintonia_player::api_version();
    log::info!(
        "Sintonia {} con libmpv API {major}.{minor}",
        env!("CARGO_PKG_VERSION")
    );

    let main = MainWindow::new()?;
    let pip = PipWindow::new()?;

    let config = PlayerConfig {
        log_level: if log::log_enabled!(target: "mpv", log::Level::Debug) {
            "v"
        } else {
            "warn"
        },
        ..PlayerConfig::default()
    };
    let player = Rc::new(Player::new(&config, video::forward_player_event)?);
    let router = VideoRouter::new(Rc::clone(&player), &main, &pip);
    router.install_main(&main)?;
    router.install_pip(&pip)?;

    let stream_override = demo::override_from_env();
    let url_for = {
        let stream_override = stream_override.clone();
        move |index: usize| demo::stream_for(index, stream_override.as_deref())
    };

    // cambio di canale (zapping, scelta dall'elenco, Guarda)
    {
        let router = Rc::clone(&router);
        let url_for = url_for.clone();
        main.on_tune(move |index| {
            if let Ok(index) = usize::try_from(index) {
                router.tune(index, url_for(index), ZAP_DELAY);
            }
        });
    }

    {
        let main_weak = main.as_weak();
        main.on_toggle_fullscreen(move || {
            if let Some(main) = main_weak.upgrade() {
                let window = main.window();
                window.set_fullscreen(!window.is_fullscreen());
            }
        });
    }

    // mini player: si apre in basso a destra della finestra principale
    {
        let pip_weak = pip.as_weak();
        let main_weak = main.as_weak();
        let router = Rc::clone(&router);
        main.on_request_pip(move |ch| {
            let (Some(pip), Some(main)) = (pip_weak.upgrade(), main_weak.upgrade()) else {
                return;
            };
            if let Some(index) = main.get_channels().iter().position(|c| c.num == ch.num) {
                // se è un altro canale, il cambio lo fa il gestore `changed selected`
                main.set_selected(index as i32);
            }
            pip.set_num(ch.num);
            pip.set_name(ch.name);
            pip.set_now(ch.now);
            pip.set_tint(ch.tint);
            if let Err(err) = pip.show() {
                log::error!("apertura del mini player: {err}");
                return;
            }
            let (pos, size) = (main.window().position(), main.window().size());
            let scale = pip.window().scale_factor();
            pip.window().set_position(PhysicalPosition::new(
                pos.x + size.width as i32 - (400.0 * scale) as i32,
                pos.y + size.height as i32 - (260.0 * scale) as i32,
            ));
            router.move_to_pip();
        });
    }
    {
        let router = Rc::clone(&router);
        main.on_close_pip(move || router.move_to_main());
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
    // «chiudi»: chiude il mini player e ferma la riproduzione
    {
        let router = Rc::clone(&router);
        pip.on_dismiss(move || {
            router.stop();
            router.move_to_main();
        });
    }
    // «espandi»: riprende il canale a tutto schermo nella finestra principale
    {
        let main_weak = main.as_weak();
        let router = Rc::clone(&router);
        pip.on_expand(move || {
            let Some(main) = main_weak.upgrade() else {
                return;
            };
            router.move_to_main();
            main.set_playing(true);
            main.window().set_minimized(false);
            if let Err(err) = main.show() {
                log::error!("{err}");
            }
        });
    }

    // chiudere la finestra principale chiude l'app, anche con il mini player aperto
    main.window().on_close_requested(|| {
        if let Err(err) = slint::quit_event_loop() {
            log::error!("{err}");
        }
        slint::CloseRequestResponse::HideWindow
    });

    // all'avvio l'anteprima della Home mostra il canale selezionato
    if let Ok(index) = usize::try_from(main.get_selected()) {
        router.tune(index, url_for(index), Duration::ZERO);
    }

    main.show()?;
    slint::run_event_loop()?;
    router.stop();
    pip.hide()?;
    main.hide()?;
    Ok(())
}
