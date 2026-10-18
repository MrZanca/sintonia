//! Il regista dell'app: collega l'interfaccia, il servizio (rete, disco, archivio) e il player.
//!
//! Tutto qui gira sul thread dell'interfaccia. Gli eventi del servizio arrivano da altri thread e
//! vengono inoltrati con `slint::invoke_from_event_loop` a [`App::on_service_event`].

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use sintonia_core::db::{NewSource, SourceInfo, SourceKind};
use sintonia_core::service::{Command, Event, LogoImage, Service, SourceError, SourceReport};
use sintonia_player::TrackKind;
use slint::{ComponentHandle, Model, ModelRc, PhysicalPosition, SharedString, VecModel};

use crate::channels::ChannelModel;
use crate::settings::{Language, Settings};
use crate::video::VideoRouter;
use crate::{CategoryItem, MainWindow, PipWindow, SourceItem, SourceStatus, TrackItem};

/// Attesa prima di aprire il canale evidenziato nell'elenco Live (anteprima).
const PREVIEW_DELAY: Duration = Duration::from_millis(700);
/// Attesa prima di cercare mentre si scrive.
const SEARCH_DELAY: Duration = Duration::from_millis(150);
/// Categorie mostrate nella Home.
const HOME_CATEGORIES: usize = 5;
/// Mini player: larghezza iniziale, minima e massima (pixel logici) e proporzioni.
const PIP_WIDTH: f32 = 360.0;
const PIP_MIN_WIDTH: f32 = 240.0;
const PIP_MAX_WIDTH: f32 = 1600.0;
const PIP_RATIO: f32 = 9.0 / 16.0;
/// Margine del mini player dal bordo della finestra principale, all'apertura.
const PIP_MARGIN: f32 = 40.0;
/// Spostamento di uno scatto della rotellina, come lo riporta Slint (pixel logici).
const WHEEL_NOTCH: f32 = 60.0;

/// Punto che resta fermo quando il mini player cambia dimensione.
#[derive(Debug, Clone, Copy)]
enum PipAnchor {
    /// L'angolo opposto a quello trascinato (`left`/`top`: lato dell'angolo trascinato).
    Corner { left: bool, top: bool },
    /// Il centro (rotellina del mouse).
    Center,
}

thread_local! {
    static APP: RefCell<Weak<App>> = const { RefCell::new(Weak::new()) };
}

/// Inoltra un evento del servizio al thread dell'interfaccia (chiamata da thread del servizio).
pub fn forward_service_event(event: Event) {
    let sent = slint::invoke_from_event_loop(move || {
        APP.with(|app| {
            if let Some(app) = app.borrow().upgrade() {
                app.on_service_event(event);
            }
        });
    });
    if sent.is_err() {
        log::trace!("evento del servizio perso alla chiusura");
    }
}

/// Stato dell'app sul thread dell'interfaccia.
pub struct App {
    main: slint::Weak<MainWindow>,
    pip: slint::Weak<PipWindow>,
    service: Service,
    router: Rc<VideoRouter>,
    channels: Rc<ChannelModel>,
    settings: RefCell<Settings>,
    source_id: Cell<Option<i64>>,
    /// La prima volta che arrivano i canali si riprende l'ultimo canale visto.
    started: Cell<bool>,
    /// Canale mostrato come «in onda» (anche se fermo).
    current: Cell<Option<i64>>,
    stream_token: Cell<u64>,
    search_generation: Cell<u64>,
    search_timer: slint::Timer,
    preview_timer: slint::Timer,
    clock_timer: slint::Timer,
    aspect: Cell<i32>,
    /// Lo schermo intero l'ha acceso l'apertura del player (e la chiusura lo spegne).
    auto_fullscreen: Cell<bool>,
    /// Salva la dimensione del mini player poco dopo l'ultimo ridimensionamento.
    pip_save_timer: slint::Timer,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("source_id", &self.source_id.get())
            .field("current", &self.current.get())
            .finish_non_exhaustive()
    }
}

impl App {
    /// Crea l'app e collega i callback delle finestre. Il servizio deve già essere avviato.
    pub fn new(
        main: &MainWindow,
        pip: &PipWindow,
        service: Service,
        router: Rc<VideoRouter>,
        language: Language,
    ) -> Rc<Self> {
        // il servizio è dentro l'app: le richieste dei loghi passano da lì
        let channels = ChannelModel::new(|url| {
            APP.with(|app| {
                if let Some(app) = app.borrow().upgrade() {
                    app.service.send(Command::FetchLogo(url));
                }
            });
        });
        main.set_channels(ModelRc::from(Rc::clone(&channels)));

        let app = Rc::new(Self {
            main: main.as_weak(),
            pip: pip.as_weak(),
            service,
            router,
            channels,
            settings: RefCell::new(Settings::defaults(language)),
            source_id: Cell::new(None),
            started: Cell::new(false),
            current: Cell::new(None),
            stream_token: Cell::new(0),
            search_generation: Cell::new(0),
            search_timer: slint::Timer::default(),
            preview_timer: slint::Timer::default(),
            clock_timer: slint::Timer::default(),
            aspect: Cell::new(0),
            auto_fullscreen: Cell::new(false),
            pip_save_timer: slint::Timer::default(),
        });
        APP.with(|a| *a.borrow_mut() = Rc::downgrade(&app));
        app.apply_language(language);
        app.connect(main, pip);
        app.start_clock();
        app.service.send(Command::Init);
        app
    }

    fn main(&self) -> Option<MainWindow> {
        self.main.upgrade()
    }

    /// Ferma la riproduzione (alla chiusura).
    pub fn shutdown(&self) {
        self.router.stop();
    }

    // ───────────── eventi del servizio ─────────────

    fn on_service_event(self: &Rc<Self>, event: Event) {
        match event {
            Event::Settings(values) => {
                let language = self.settings.borrow().language;
                self.apply_settings(Settings::from_pairs(&values, language));
            }
            Event::Sources(sources) => self.show_sources(&sources),
            Event::Channels {
                source_id,
                channels,
            } => {
                let changed_source = self.source_id.replace(source_id) != source_id;
                self.channels.set_channels(channels);
                self.after_channels_changed(changed_source);
            }
            Event::Working(working) => {
                if let Some(main) = self.main() {
                    main.set_working(working);
                    if working {
                        main.set_source_status(SourceStatus {
                            state: 1,
                            ..Default::default()
                        });
                    }
                }
            }
            Event::Verified(result) => self.show_source_result(result, false),
            Event::SourceSaved { result, .. } => self.show_source_result(result, true),
            Event::SearchResults { generation, ids } => {
                if generation == self.search_generation.get() {
                    self.channels.show_search(&ids);
                    self.after_filter_changed();
                }
            }
            Event::Stream {
                token,
                channel_id,
                result,
            } => {
                if token != self.stream_token.get() {
                    return;
                }
                match result {
                    Ok(stream) => {
                        self.router.play(channel_id, stream);
                        if let Some(name) = self.channels.name_of(channel_id) {
                            self.save_setting("last_channel", &name);
                        }
                    }
                    Err(err) => log::error!("canale non apribile: {err}"),
                }
            }
            Event::Logo { url, image } => {
                self.channels.logo_ready(url, image.map(to_slint_image));
            }
            Event::Error(message) => log::error!("{message}"),
        }
    }

    fn apply_settings(&self, settings: Settings) {
        self.apply_language(settings.language);
        self.router.set_reconnect(settings.reconnect);
        if let Err(err) = self
            .router
            .player()
            .set_user_agent(settings.user_agent.as_deref())
        {
            log::warn!("{err}");
        }
        if let Some(main) = self.main() {
            main.set_reconnect(settings.reconnect);
            main.set_fullscreen_start(settings.fullscreen_start);
            main.set_user_agent(settings.user_agent.clone().unwrap_or_default().into());
        }
        *self.settings.borrow_mut() = settings;
    }

    fn apply_language(&self, language: Language) {
        if let Err(err) = slint::select_bundled_translation(language.code()) {
            log::warn!("lingua {}: {err}", language.code());
        }
        if let Some(main) = self.main() {
            main.set_language(language.index());
        }
    }

    fn show_sources(&self, sources: &[SourceInfo]) {
        let Some(main) = self.main() else { return };
        let items: Vec<SourceItem> = sources
            .iter()
            .map(|s| SourceItem {
                id: i32::try_from(s.id).unwrap_or(-1),
                name: s.name.as_str().into(),
                detail: s.display.as_str().into(),
                count: i32::try_from(s.channel_count).unwrap_or(i32::MAX),
                active: s.active,
                file: s.kind == SourceKind::M3uFile,
            })
            .collect();
        main.set_no_source(items.is_empty());
        main.set_sources(ModelRc::new(VecModel::from(items)));
    }

    fn show_source_result(&self, result: Result<SourceReport, SourceError>, saved: bool) {
        let Some(main) = self.main() else { return };
        let status = match result {
            Ok(report) => SourceStatus {
                state: if saved { 3 } else { 2 },
                channels: count_i32(report.channels),
                groups: count_i32(report.groups),
                skipped: count_i32(report.skipped),
                ..Default::default()
            },
            Err(err) => source_error_status(&err),
        };
        main.set_source_status(status);
    }

    fn after_channels_changed(self: &Rc<Self>, changed_source: bool) {
        let Some(main) = self.main() else { return };
        let categories = self.channels.categories();
        let top: Vec<CategoryItem> = categories
            .iter()
            .skip(1)
            .take(HOME_CATEGORIES)
            .cloned()
            .collect();
        main.set_categories(ModelRc::new(VecModel::from(categories)));
        main.set_top_categories(ModelRc::new(VecModel::from(top)));
        main.set_category(0);
        if !main.get_search().is_empty() {
            main.set_search(SharedString::new());
        }

        // il canale in onda dopo un aggiornamento ha un nuovo identificativo: lo si ritrova per nome
        let previous_name = self.settings.borrow().last_channel.clone();
        let current = if changed_source {
            None
        } else {
            self.current
                .get()
                .and_then(|id| self.channels.item(id).map(|_| id))
        };
        let current = current
            .or_else(|| {
                previous_name
                    .as_deref()
                    .and_then(|n| self.channels.find_by_name(n))
            })
            .or_else(|| self.channels.first_id());
        if changed_source && self.router.playing_channel().is_some() {
            self.router.stop();
        }
        self.set_current(current);
        self.after_filter_changed();

        // all'avvio riprende l'ultimo canale (l'anteprima della Home è «in onda»)
        if !self.started.replace(true) {
            if let Some(id) = current {
                self.play_channel(id);
            }
        }
    }

    fn after_filter_changed(&self) {
        let Some(main) = self.main() else { return };
        main.set_category(i32::try_from(self.channels.category()).unwrap_or(0));
        let row = self
            .current
            .get()
            .and_then(|id| self.channels.row_of(id))
            .unwrap_or(0);
        main.set_cursor(i32::try_from(row).unwrap_or(0));
    }

    fn set_current(&self, id: Option<i64>) {
        self.current.set(id);
        let Some(main) = self.main() else { return };
        let item = id.and_then(|id| self.channels.item(id));
        main.set_has_current(item.is_some());
        let item = item.unwrap_or_default();
        if let Some(pip) = self.pip.upgrade() {
            pip.set_num(item.num.clone());
            pip.set_name(item.name.clone());
            pip.set_tint(item.tint);
        }
        main.set_current(item);
    }

    /// Chiede al servizio l'indirizzo del canale; si apre quando arriva (evento `Stream`).
    fn play_channel(&self, id: i64) {
        self.preview_timer.stop();
        self.set_current(Some(id));
        if self.router.playing_channel() == Some(id) {
            return;
        }
        let token = self.stream_token.get() + 1;
        self.stream_token.set(token);
        self.service.send(Command::ResolveStream {
            channel_id: id,
            token,
        });
    }

    fn save_setting(&self, key: &str, value: &str) {
        self.settings.borrow_mut().set(key, value);
        self.service.send(Command::SetSetting {
            key: key.to_owned(),
            value: value.to_owned(),
        });
    }

    fn start_clock(self: &Rc<Self>) {
        let update = {
            let main = self.main.clone();
            move || {
                if let Some(main) = main.upgrade() {
                    main.set_clock(chrono::Local::now().format("%H:%M").to_string().into());
                }
            }
        };
        update();
        self.clock_timer
            .start(slint::TimerMode::Repeated, Duration::from_secs(5), update);
    }

    // ───────────── comandi dell'interfaccia ─────────────

    fn connect(self: &Rc<Self>, main: &MainWindow, pip: &PipWindow) {
        let weak = Rc::downgrade(self);
        // ogni callback tiene solo un riferimento debole all'app
        let on = |f: fn(&Rc<App>)| {
            let weak = weak.clone();
            move || {
                if let Some(app) = weak.upgrade() {
                    f(&app);
                }
            }
        };
        macro_rules! with_app {
            (|$app:ident $(, $arg:ident : $ty:ty)*| $body:expr) => {{
                let weak = weak.clone();
                move |$($arg: $ty),*| {
                    if let Some($app) = weak.upgrade() {
                        $body
                    }
                }
            }};
        }

        main.on_play_index(with_app!(|app, row: i32| {
            if let Some(id) = usize::try_from(row)
                .ok()
                .and_then(|r| app.channels.id_at(r))
            {
                app.play_channel(id);
            }
        }));
        main.on_cursor_moved(with_app!(|app, row: i32| app.preview_later(row)));
        main.on_zap(with_app!(|app, delta: i32| app.zap(delta)));
        main.on_pick_category(with_app!(|app, index: i32| {
            if let Some(main) = app.main() {
                main.set_search(SharedString::new());
            }
            app.search_generation.set(app.search_generation.get() + 1);
            app.channels
                .show_category(usize::try_from(index).unwrap_or(0));
            app.after_filter_changed();
        }));
        main.on_search_edited(with_app!(
            |app, text: SharedString| app.search_later(text.to_string())
        ));
        main.on_request_pip(with_app!(|app, row: i32| app.open_pip(row)));
        main.on_close_pip(on(|app| app.router.move_to_main()));
        main.on_toggle_fullscreen(on(|app| {
            if let Some(main) = app.main() {
                let window = main.window();
                window.set_fullscreen(!window.is_fullscreen());
                app.auto_fullscreen.set(false);
            }
        }));
        main.on_playing_changed(with_app!(
            |app, playing: bool| app.on_playing_changed(playing)
        ));
        main.on_verify_source(with_app!(
            |app, kind: i32, name: SharedString, location: SharedString, ua: SharedString| {
                app.submit_source(kind, &name, &location, &ua, false)
            }
        ));
        main.on_save_source(with_app!(
            |app, kind: i32, name: SharedString, location: SharedString, ua: SharedString| {
                app.submit_source(kind, &name, &location, &ua, true)
            }
        ));
        main.on_pick_file(on(|app| app.pick_file()));
        main.on_activate_source(with_app!(|app, id: i32| app
            .service
            .send(Command::ActivateSource(i64::from(id)))));
        main.on_refresh_source(with_app!(|app, id: i32| app
            .service
            .send(Command::RefreshSource(i64::from(id)))));
        main.on_delete_source(with_app!(|app, id: i32| app
            .service
            .send(Command::DeleteSource(i64::from(id)))));
        main.on_set_language(with_app!(|app, index: i32| {
            let language = Language::from_index(index);
            app.apply_language(language);
            app.save_setting("language", language.code());
        }));
        main.on_setting_changed(with_app!(|app, key: SharedString, value: SharedString| {
            app.save_setting(&key, &value);
            let settings = app.settings.borrow().clone();
            app.router.set_reconnect(settings.reconnect);
            if key == "user_agent" {
                if let Err(err) = app
                    .router
                    .player()
                    .set_user_agent(settings.user_agent.as_deref())
                {
                    log::warn!("{err}");
                }
            }
        }));
        main.on_load_tracks(on(|app| app.load_tracks()));
        main.on_select_audio(with_app!(|app, id: i32| {
            if let Err(err) = app
                .router
                .player()
                .select_track(TrackKind::Audio, Some(i64::from(id)))
            {
                log::warn!("{err}");
            }
            app.load_tracks();
        }));
        main.on_select_sub(with_app!(|app, id: i32| {
            let id = (id >= 0).then_some(i64::from(id));
            if let Err(err) = app.router.player().select_track(TrackKind::Subtitle, id) {
                log::warn!("{err}");
            }
            app.load_tracks();
        }));
        main.on_cycle_aspect(on(|app| app.cycle_aspect()));

        // mini player
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
        pip.on_resize(with_app!(|app, corner: i32, dx: f32, dy: f32| {
            let (left, top) = (corner == 0 || corner == 2, corner == 0 || corner == 1);
            // la larghezza segue il movimento più ampio, l'altezza resta in proporzione
            let from_x = if left { -dx } else { dx };
            let from_y = (if top { -dy } else { dy }) / PIP_RATIO;
            let delta = if from_x.abs() >= from_y.abs() {
                from_x
            } else {
                from_y
            };
            app.resize_pip(|w| w + delta, PipAnchor::Corner { left, top });
        }));
        pip.on_resize_step(with_app!(|app, delta: f32| {
            // +10% per scatto della rotellina (uno scatto vale 60 px); più scatti possono
            // arrivare in un solo evento
            let factor = 1.1f32.powf(delta / WHEEL_NOTCH).clamp(0.5, 2.0);
            app.resize_pip(|w| w * factor, PipAnchor::Center);
        }));
        // «chiudi»: chiude il mini player e ferma la riproduzione
        pip.on_dismiss(on(|app| {
            app.router.stop();
            app.router.move_to_main();
        }));
        // «espandi»: riprende il canale a tutto schermo nella finestra principale
        pip.on_expand(on(|app| {
            let Some(main) = app.main() else { return };
            app.router.move_to_main();
            main.set_playing(true);
            main.window().set_minimized(false);
            if let Err(err) = main.show() {
                log::error!("{err}");
            }
        }));
    }

    fn preview_later(self: &Rc<Self>, row: i32) {
        // con il mini player aperto l'elenco non cambia il canale in onda
        if self.pip.upgrade().is_some_and(|p| p.window().is_visible()) {
            return;
        }
        let Some(id) = usize::try_from(row)
            .ok()
            .and_then(|r| self.channels.id_at(r))
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        self.preview_timer
            .start(slint::TimerMode::SingleShot, PREVIEW_DELAY, move || {
                if let Some(app) = weak.upgrade() {
                    app.play_channel(id);
                }
            });
    }

    fn zap(&self, delta: i32) {
        let count = self.channels.row_count();
        if count == 0 {
            return;
        }
        let row = self
            .current
            .get()
            .and_then(|id| self.channels.row_of(id))
            .map_or(0, |r| i64::try_from(r).unwrap_or(0));
        let count_i = i64::try_from(count).unwrap_or(i64::MAX);
        let next = (row + i64::from(delta)).rem_euclid(count_i);
        let next = usize::try_from(next).unwrap_or(0);
        if let Some(id) = self.channels.id_at(next) {
            self.play_channel(id);
            if let Some(main) = self.main() {
                main.set_cursor(i32::try_from(next).unwrap_or(0));
            }
        }
    }

    fn search_later(self: &Rc<Self>, text: String) {
        let generation = self.search_generation.get() + 1;
        self.search_generation.set(generation);
        if text.trim().is_empty() {
            self.search_timer.stop();
            let category = self.channels.category();
            self.channels.show_category(category);
            self.after_filter_changed();
            return;
        }
        let weak = Rc::downgrade(self);
        self.search_timer
            .start(slint::TimerMode::SingleShot, SEARCH_DELAY, move || {
                if let Some(app) = weak.upgrade() {
                    app.service.send(Command::Search {
                        query: text.clone(),
                        generation,
                    });
                }
            });
    }

    fn open_pip(&self, row: i32) {
        let (Some(main), Some(pip)) = (self.main(), self.pip.upgrade()) else {
            return;
        };
        let id = usize::try_from(row)
            .ok()
            .and_then(|r| self.channels.id_at(r))
            .or(self.current.get());
        let Some(id) = id else { return };
        self.play_channel(id);
        let width = self
            .settings
            .borrow()
            .pip_width
            .unwrap_or(PIP_WIDTH)
            .clamp(PIP_MIN_WIDTH, PIP_MAX_WIDTH);
        pip.window()
            .set_size(slint::LogicalSize::new(width, width * PIP_RATIO));
        if let Err(err) = pip.show() {
            log::error!("apertura del mini player: {err}");
            return;
        }
        // in basso a destra della finestra principale
        let (pos, size) = (main.window().position(), main.window().size());
        let scale = pip.window().scale_factor();
        let margin = PIP_MARGIN * scale;
        pip.window().set_position(PhysicalPosition::new(
            pos.x + size.width as i32 - ((width + PIP_MARGIN) * scale) as i32,
            pos.y + size.height as i32 - (width * PIP_RATIO * scale + margin) as i32,
        ));
        self.router.move_to_pip();
    }

    /// Cambia la larghezza del mini player (l'altezza segue in 16:9) tenendo fermo `anchor`.
    fn resize_pip(self: &Rc<Self>, new_width: impl FnOnce(f32) -> f32, anchor: PipAnchor) {
        let Some(pip) = self.pip.upgrade() else {
            return;
        };
        let window = pip.window();
        let scale = window.scale_factor();
        let size = window.size().to_logical(scale);
        let width = new_width(size.width).clamp(PIP_MIN_WIDTH, PIP_MAX_WIDTH);
        let height = width * PIP_RATIO;
        let (dw, dh) = (width - size.width, height - size.height);
        if dw.abs() < 0.5 {
            return;
        }
        let pos = window.position();
        window.set_size(slint::LogicalSize::new(width, height));
        let (shift_x, shift_y) = match anchor {
            PipAnchor::Corner { left, top } => {
                (if left { dw } else { 0.0 }, if top { dh } else { 0.0 })
            }
            PipAnchor::Center => (dw / 2.0, dh / 2.0),
        };
        if shift_x != 0.0 || shift_y != 0.0 {
            window.set_position(PhysicalPosition::new(
                pos.x - (shift_x * scale).round() as i32,
                pos.y - (shift_y * scale).round() as i32,
            ));
        }
        let weak = Rc::downgrade(self);
        self.pip_save_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(600),
            move || {
                if let Some(app) = weak.upgrade() {
                    app.save_setting("pip_width", &format!("{width:.0}"));
                }
            },
        );
    }

    fn on_playing_changed(&self, playing: bool) {
        let Some(main) = self.main() else { return };
        let window = main.window();
        if playing && self.settings.borrow().fullscreen_start && !window.is_fullscreen() {
            window.set_fullscreen(true);
            self.auto_fullscreen.set(true);
        } else if !playing && self.auto_fullscreen.replace(false) {
            window.set_fullscreen(false);
        }
        if playing {
            // il player riprende il canale in onda, se era fermo
            if let Some(id) = self.current.get() {
                self.play_channel(id);
            }
        }
    }

    fn submit_source(&self, kind: i32, name: &str, location: &str, user_agent: &str, save: bool) {
        let Some(main) = self.main() else { return };
        let location = location.trim();
        if location.is_empty() {
            main.set_source_status(SourceStatus {
                state: 4,
                error: 7,
                ..Default::default()
            });
            return;
        }
        let kind = if kind == 1 {
            SourceKind::M3uFile
        } else {
            SourceKind::M3uUrl
        };
        let name = match name.trim() {
            "" => "Playlist".to_owned(),
            name => name.to_owned(),
        };
        let user_agent = Some(user_agent.trim())
            .filter(|ua| !ua.is_empty())
            .map(str::to_owned);
        let source = NewSource {
            name,
            kind,
            location: location.to_owned(),
            user_agent,
        };
        self.service.send(if save {
            Command::AddSource(source)
        } else {
            Command::VerifySource(source)
        });
    }

    fn pick_file(&self) {
        let main = self.main.clone();
        let picked = slint::spawn_local(async move {
            let file = rfd::AsyncFileDialog::new()
                .add_filter("M3U", &["m3u", "m3u8"])
                .pick_file()
                .await;
            if let (Some(file), Some(main)) = (file, main.upgrade()) {
                main.set_picked_file(file.path().to_string_lossy().as_ref().into());
            }
        });
        if let Err(err) = picked {
            log::error!("selettore di file: {err}");
        }
    }

    fn load_tracks(&self) {
        let Some(main) = self.main() else { return };
        let tracks = self.router.player().tracks();
        let label = |t: &sintonia_player::Track, n: usize| -> SharedString {
            let mut parts: Vec<String> = Vec::new();
            if let Some(lang) = &t.lang {
                parts.push(lang.to_uppercase());
            }
            if let Some(title) = &t.title {
                parts.push(title.clone());
            }
            if let Some(codec) = &t.codec {
                parts.push(codec.to_uppercase());
            }
            if parts.is_empty() {
                format!("#{n}").into()
            } else {
                parts.join(" · ").into()
            }
        };
        let pick = |kind: TrackKind| -> Vec<TrackItem> {
            tracks
                .iter()
                .filter(|t| t.kind == kind)
                .enumerate()
                .map(|(n, t)| TrackItem {
                    id: i32::try_from(t.id).unwrap_or(-1),
                    label: label(t, n + 1),
                    selected: t.selected,
                })
                .collect()
        };
        let audio = pick(TrackKind::Audio);
        let subs = pick(TrackKind::Subtitle);
        main.set_any_sub_selected(subs.iter().any(|t| t.selected));
        // con una sola traccia audio non c'è niente da scegliere
        main.set_audio_tracks(ModelRc::new(VecModel::from(if audio.len() > 1 {
            audio
        } else {
            Vec::new()
        })));
        main.set_sub_tracks(ModelRc::new(VecModel::from(subs)));
    }

    fn cycle_aspect(&self) {
        let next = (self.aspect.get() + 1) % 4;
        self.aspect.set(next);
        let (ratio, panscan) = match next {
            1 => ("16:9", "0"),
            2 => ("4:3", "0"),
            3 => ("-1", "1.0"),
            _ => ("-1", "0"),
        };
        let player = self.router.player();
        for (name, value) in [("video-aspect-override", ratio), ("panscan", panscan)] {
            if let Err(err) = player.set_option(name, value) {
                log::warn!("{err}");
            }
        }
        if let Some(main) = self.main() {
            main.set_aspect(next);
        }
    }
}

fn count_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

fn source_error_status(err: &SourceError) -> SourceStatus {
    let (error, http, detail) = match err {
        SourceError::Http(code) => (1, i32::from(*code), String::new()),
        SourceError::Network(d) => (2, 0, d.clone()),
        SourceError::File(d) => (3, 0, d.clone()),
        SourceError::Empty => (4, 0, String::new()),
        SourceError::TooLarge => (5, 0, String::new()),
        SourceError::Store(d) => (6, 0, d.clone()),
    };
    SourceStatus {
        state: 4,
        error,
        http,
        detail: detail.into(),
        ..Default::default()
    }
}

fn to_slint_image(logo: LogoImage) -> slint::Image {
    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        &logo.rgba,
        logo.width,
        logo.height,
    );
    slint::Image::from_rgba8(buffer)
}
