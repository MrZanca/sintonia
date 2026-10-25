//! Sintonia: player IPTV per PC.
//!
//! Avvio: cartelle dei dati, chiave dell'archivio dal portachiavi, servizio (I/O su tokio),
//! player (libmpv), finestre Slint. La logica dell'interfaccia sta in [`app`], il video in [`video`].

// Niente console in release su Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod awake;
mod catalog;
mod guide;
mod images;
mod keys;
mod mediakeys;
mod remote;
mod settings;
mod video;
mod zoom;

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

/// Profilo di prova (`SINTONIA_PROFILO`, solo lettere, cifre, `-` e `_`): archivio, cache e voci
/// del portachiavi tutti a parte, così le prove non toccano i dati veri.
fn test_profile() -> Option<String> {
    std::env::var("SINTONIA_PROFILO").ok().filter(|p| {
        !p.is_empty()
            && p.len() <= 32
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// Cartelle di Sintonia: `%APPDATA%\Sintonia` (archivio) e `%LOCALAPPDATA%\Sintonia` (cache);
/// con un profilo di prova, le sottocartelle `profili\<profilo>`.
fn data_dirs(profile: Option<&str>) -> Result<(PathBuf, PathBuf), AppError> {
    let dirs = directories::ProjectDirs::from("", "", "Sintonia").ok_or(AppError::NoDataDir)?;
    let (data, cache) = (
        dirs.data_dir().to_path_buf(),
        dirs.cache_dir().to_path_buf(),
    );
    Ok(match profile {
        Some(p) => (data.join("profili").join(p), cache.join("profili").join(p)),
        None => (data, cache),
    })
}

/// Apre in esclusiva `sintonia.lock` nella cartella dei dati: finché il file resta aperto (cioè
/// finché il processo vive, anche se crolla) una seconda Sintonia non parte. `None` se è già
/// aperto da un'altra Sintonia; se la cartella non si può usare si parte comunque.
fn single_instance(data_dir: &Path) -> Option<Option<std::fs::File>> {
    if let Err(err) = std::fs::create_dir_all(data_dir) {
        log::warn!("cartella dei dati: {err}");
        return Some(None);
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // nessuna condivisione: un'altra apertura fallisce con «violazione di condivisione»
        options.share_mode(0);
    }
    match options.open(data_dir.join("sintonia.lock")) {
        Ok(file) => Some(Some(file)),
        // 32 = ERROR_SHARING_VIOLATION
        Err(err) if err.raw_os_error() == Some(32) => None,
        Err(err) => {
            log::warn!("blocco dell'istanza: {err}");
            Some(None)
        }
    }
}

/// Collega i tasti del volume (telecomandi, tastiere multimediali) al volume del player.
fn install_volume_keys(weak: slint::Weak<MainWindow>, retries: u32) {
    let delay = std::time::Duration::from_millis(if retries > 0 { 300 } else { 2000 });
    slint::Timer::single_shot(delay, move || {
        let Some(main) = weak.upgrade() else { return };
        let handler_weak = weak.clone();
        let installed = mediakeys::install(main.window(), move |key, repeat| {
            let weak = handler_weak.clone();
            // dopo il messaggio di Windows in corso
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                let Some(main) = weak.upgrade() else { return };
                let video = main.global::<Video>();
                match key {
                    // tenendo premuto il muto non deve accendersi e spegnersi di continuo
                    mediakeys::VolumeKey::Mute if !repeat => video.invoke_toggle_mute(),
                    mediakeys::VolumeKey::Mute => {}
                    _ => {
                        let volume = video.get_volume() + mediakeys::step(key, repeat);
                        video.invoke_set_volume(volume.clamp(0, 100));
                    }
                }
            });
        });
        if !installed && retries > 0 {
            install_volume_keys(weak, retries - 1);
        }
    });
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

    let profile = test_profile();
    if let Some(p) = &profile {
        log::warn!("profilo di prova «{p}»: dati e portachiavi a parte");
    }
    let service = profile.as_ref().map_or_else(
        || secrets::SERVICE.to_owned(),
        |p| format!("{}-{p}", secrets::SERVICE),
    );
    let (data_dir, cache_dir) = data_dirs(profile.as_deref())?;
    // una sola Sintonia per archivio: due insieme si contendono l'archivio
    let Some(_instance) = single_instance(&data_dir) else {
        log::warn!("Sintonia è già aperta");
        let _ = rfd::MessageDialog::new()
            .set_title("Sintonia")
            .set_description(match Language::system() {
                Language::Italian => "Sintonia è già aperta.",
                Language::English => "Sintonia is already open.",
            })
            .set_level(rfd::MessageLevel::Info)
            .show();
        return Ok(());
    };
    let database = data_dir.join("archivio.db");
    let (key, created) = secrets::database_key(&service)?;
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
            secrets: Arc::new(SystemKeyring { service }),
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
    remote::install(&main);

    // chiudere la finestra principale chiude l'app, anche con il mini player aperto
    main.window().on_close_requested(|| {
        if let Err(err) = slint::quit_event_loop() {
            log::error!("{err}");
        }
        slint::CloseRequestResponse::HideWindow
    });

    main.show()?;
    // i tasti del volume dei telecomandi: la finestra di Windows nasce con il ciclo degli eventi,
    // quindi si collegano poco dopo (e si riprova una volta)
    install_volume_keys(main.as_weak(), 1);
    slint::run_event_loop()?;
    app.shutdown();
    pip.hide()?;
    main.hide()?;
    Ok(())
}
