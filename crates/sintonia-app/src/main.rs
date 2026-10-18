//! Sintonia: player IPTV per PC.
//!
//! Avvio: cartelle dei dati, chiave dell'archivio dal portachiavi, servizio (I/O su tokio),
//! player (libmpv), finestre Slint. La logica dell'interfaccia sta in [`app`], il video in [`video`].

// Niente console in release su Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod awake;
mod catalog;
mod images;
mod settings;
mod video;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;

use sintonia_core::crypto::Cipher;
use sintonia_core::secrets::{self, SecretsError, SystemKeyring};
use sintonia_core::service::{Service, ServiceConfig, ServiceError};
use sintonia_player::{Player, PlayerConfig, PlayerError};
use slint::ComponentHandle;

use crate::app::App;
use crate::settings::Language;
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
    #[error("{0}")]
    Service(#[from] ServiceError),
    #[error("{0}")]
    Secrets(#[from] SecretsError),
    #[error("cartella dei dati non trovata")]
    NoDataDir,
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

/// Cartelle di Sintonia: `%APPDATA%\Sintonia` (archivio) e `%LOCALAPPDATA%\Sintonia` (cache).
fn data_dirs() -> Result<(PathBuf, PathBuf), AppError> {
    let dirs = directories::ProjectDirs::from("", "", "Sintonia").ok_or(AppError::NoDataDir)?;
    Ok((
        dirs.data_dir().to_path_buf(),
        dirs.cache_dir().to_path_buf(),
    ))
}

/// Se la chiave è nuova, un archivio vecchio non sarebbe più leggibile: lo si mette da parte.
fn set_aside_old_database(database: &Path) {
    if !database.exists() {
        return;
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let aside = database.with_file_name(format!("archivio-{stamp}.vecchio"));
    log::warn!("chiave dell'archivio nuova: l'archivio precedente viene messo da parte");
    for suffix in ["", "-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{suffix}", database.display()));
        if from.exists() {
            let to = PathBuf::from(format!("{}{suffix}", aside.display()));
            if let Err(err) = std::fs::rename(&from, &to) {
                log::error!("archivio non spostato: {err}");
            }
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

    let (data_dir, cache_dir) = data_dirs()?;
    let database = data_dir.join("archivio.db");
    let (key, created) = secrets::database_key()?;
    if created {
        set_aside_old_database(&database);
    }

    let main = MainWindow::new()?;
    let pip = PipWindow::new()?;

    let service = Service::start(
        ServiceConfig {
            database,
            cache_dir,
            cipher: Cipher::new(&key),
            secrets: Arc::new(SystemKeyring),
            user_agent: format!("Sintonia/{}", env!("CARGO_PKG_VERSION")),
        },
        app::forward_service_event,
    )?;

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

    let app = App::new(&main, &pip, service, Rc::clone(&router), Language::system());

    // chiudere la finestra principale chiude l'app, anche con il mini player aperto
    main.window().on_close_requested(|| {
        if let Err(err) = slint::quit_event_loop() {
            log::error!("{err}");
        }
        slint::CloseRequestResponse::HideWindow
    });

    main.show()?;
    slint::run_event_loop()?;
    app.shutdown();
    pip.hide()?;
    main.hide()?;
    Ok(())
}
