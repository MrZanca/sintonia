//! Il regista dell'app: collega l'interfaccia, il servizio (rete, disco, archivio) e il player.
//!
//! Tutto qui gira sul thread dell'interfaccia. Gli eventi del servizio arrivano da altri thread e
//! vengono inoltrati con `slint::invoke_from_event_loop` a [`App::on_service_event`].

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use sintonia_core::db::{
    ChannelRow, EpisodeRef, HistoryEntry, ItemKind, MovieRow, NewSource, SeriesRow, SourceInfo,
    SourceKind,
};
use sintonia_core::db::{GuideProgramme, Health};
use sintonia_core::pin;
use sintonia_core::service::{
    Command, EpgState, Event, LogoImage, Playable, ResolvedStream, Service, SourceError,
    SourceReport,
};
use sintonia_core::update::{self, Update};
use sintonia_core::xtream::{Episode, MovieDetail, SeriesDetail};
use sintonia_player::TrackKind;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Model, ModelRc, PhysicalPosition, SharedString, VecModel};

use crate::catalog::{
    format_number, format_rating, tint_for, CatalogModel, PosterEntry, FAVORITES,
};
use crate::guide::{self, GuideModel, Schedule};
use crate::images;
use crate::settings::{self, Language, Locks, Settings};
use crate::video::VideoRouter;
use crate::{
    CategoryItem, ChannelItem, DetailInfo, EpisodeItem, HomeItem, MainWindow, PipWindow,
    SourceItem, SourceStatus, Texts, TrackItem,
};

/// Attesa prima di aprire il canale evidenziato nell'elenco Live (anteprima).
const PREVIEW_DELAY: Duration = Duration::from_millis(1500);
/// Aggiornamento di «in onda / dopo» e della linea «adesso».
const GUIDE_TICK: Duration = Duration::from_secs(30);
/// Programmi tenuti in memoria per «in onda / dopo»: da qualche ora fa a domani (secondi).
const NOW_WINDOW_PAST: i64 = 4 * 3600;
const NOW_WINDOW_FUTURE: i64 = 28 * 3600;
/// Dopo quanto la finestra di «in onda / dopo» si ricarica (secondi).
const NOW_WINDOW_RELOAD: i64 = 3 * 3600;
/// Attesa prima di cercare mentre si scrive.
const SEARCH_DELAY: Duration = Duration::from_millis(150);
/// Categorie mostrate nella Home.
const HOME_CATEGORIES: usize = 5;
/// Schede in una riga della Home.
const HOME_ROW_ITEMS: usize = 12;
/// Canali live al massimo in «Riprendi» (il resto è per film ed episodi).
const HOME_LIVE_ITEMS: usize = 3;
/// Aggiornamento della barra di avanzamento; il punto di visione si salva ogni tanti aggiornamenti.
const PROGRESS_TICK: Duration = Duration::from_secs(1);
const SAVE_EVERY_TICKS: u32 = 10;
/// Sotto questa posizione un film riparte dall'inizio.
const RESUME_MIN_SECS: f64 = 30.0;
/// Un film è finito oltre questa frazione, o (se lungo) quando mancano meno di questi secondi.
const END_FRACTION: f64 = 0.95;
const END_MARGIN_SECS: f64 = 60.0;
/// Conto alla rovescia prima del prossimo episodio.
const NEXT_EPISODE_SECS: i32 = 10;
/// Mini player: larghezza iniziale, minima e massima (pixel logici) e proporzioni.
const PIP_WIDTH: f32 = 360.0;
const PIP_MIN_WIDTH: f32 = 240.0;
const PIP_MAX_WIDTH: f32 = 1600.0;
const PIP_RATIO: f32 = 9.0 / 16.0;
/// Margine del mini player dal bordo della finestra principale, all'apertura.
const PIP_MARGIN: f32 = 40.0;
/// Spostamento di uno scatto della rotellina, come lo riporta Slint (pixel logici).
const WHEEL_NOTCH: f32 = 60.0;
/// Intervalli di aggiornamento automatico che si possono scegliere (ore; 0 = mai).
const INTERVAL_HOURS: [u32; 6] = [0, 6, 12, 24, 72, 168];

/// Cosa fare dopo che il PIN giusto è stato inserito.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterPin {
    /// Solo sbloccare (Impostazioni › Controllo genitori).
    Unlock,
    /// Aprire una categoria dei canali.
    LiveCategory(usize),
    /// Aprire una categoria di Film e serie.
    MediaCategory(usize),
    /// Togliere il PIN (va confermato con il PIN, anche se è già stato inserito).
    RemovePin,
}

/// Trascinamento di un angolo del mini player: tutto si calcola dalla situazione alla
/// pressione, così la finestra che cambia sotto il mouse non fa tornare indietro la misura.
#[derive(Debug, Clone, Copy)]
struct PipDrag {
    left: bool,
    top: bool,
    /// Posizione della finestra alla pressione (pixel fisici).
    pos: PhysicalPosition,
    /// Larghezza alla pressione (pixel logici).
    width: f32,
    /// Mouse alla pressione, in coordinate dello schermo (pixel fisici).
    cursor: (f32, f32),
}

/// Cosa c'è nel player (o sta per esserci).
#[derive(Debug, Clone)]
struct NowPlaying {
    item: Playable,
    kind: ItemKind,
    /// Chiave stabile: del canale, del film o della serie.
    key: String,
    title: String,
    /// Categoria del canale, anno del film, «S1 · E3» dell'episodio.
    subtitle: String,
    cover: Option<String>,
}

impl NowPlaying {
    fn on_demand(&self) -> bool {
        !matches!(self.item, Playable::Channel(_))
    }

    fn history(&self, position: f64, duration: f64) -> HistoryEntry {
        HistoryEntry {
            kind: Some(self.kind),
            key: self.key.clone(),
            title: self.title.clone(),
            subtitle: self.subtitle.clone(),
            cover: self.cover.clone(),
            position,
            duration,
            watched_at: chrono::Utc::now().timestamp(),
            episode: match &self.item {
                Playable::Episode { episode, .. } => Some(episode.clone()),
                _ => None,
            },
        }
    }
}

/// Richiesta di un flusso in attesa della risposta del servizio.
#[derive(Debug)]
struct Pending {
    token: u64,
    now: NowPlaying,
    start: Option<u32>,
}

/// Scheda aperta in Film e serie.
#[derive(Debug, Clone)]
struct Detail {
    kind: ItemKind,
    id: i64,
    key: String,
    title: String,
    cover: Option<String>,
    meta: String,
    plot: String,
    credits: String,
    loading: bool,
    /// Serie: stagioni presenti e quella mostrata (indice).
    seasons: Vec<u32>,
    season: usize,
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

/// Chiede un'immagine al servizio (dai modelli degli elenchi).
fn request_image(url: String, poster: bool) {
    APP.with(|app| {
        if let Some(app) = app.borrow().upgrade() {
            app.service.send(Command::FetchImage { url, poster });
        }
    });
}

/// Stato dell'app sul thread dell'interfaccia.
pub struct App {
    main: slint::Weak<MainWindow>,
    pip: slint::Weak<PipWindow>,
    service: Service,
    router: Rc<VideoRouter>,
    settings: RefCell<Settings>,
    source_id: Cell<Option<i64>>,

    // catalogo
    channels: Rc<CatalogModel<ChannelRow>>,
    movies: Rc<CatalogModel<PosterEntry>>,
    series: Rc<CatalogModel<PosterEntry>>,
    movie_rows: RefCell<Arc<Vec<MovieRow>>>,
    series_rows: RefCell<Arc<Vec<SeriesRow>>>,
    channel_categories: Rc<VecModel<CategoryItem>>,
    media_categories: Rc<VecModel<CategoryItem>>,
    favorites: RefCell<Vec<(ItemKind, String)>>,
    history: RefCell<Vec<HistoryEntry>>,

    // Home
    home_resume: RefCell<Vec<HistoryEntry>>,
    home_favorites: RefCell<Vec<(ItemKind, String)>>,
    /// Immagini mostrate nella Home: quando arrivano, le righe si ricostruiscono.
    home_images: RefCell<HashSet<String>>,

    // Film e serie
    /// 0 film, 1 serie.
    media_mode: Cell<i32>,
    detail: RefCell<Option<Detail>>,
    series_cache: RefCell<HashMap<i64, Rc<SeriesDetail>>>,

    // riproduzione
    /// La prima volta che arriva il catalogo si riprende l'ultimo canale visto.
    started: Cell<bool>,
    /// Canale mostrato come «in onda» (anche se fermo).
    current: Cell<Option<i64>>,
    now: RefCell<Option<NowPlaying>>,
    pending: RefCell<Option<Pending>>,
    stream_token: Cell<u64>,
    /// Ultima posizione e durata lette (secondi).
    progress: Cell<(f64, f64)>,
    progress_ticks: Cell<u32>,
    progress_timer: slint::Timer,
    next_episode: RefCell<Option<NowPlaying>>,
    next_countdown: Cell<i32>,
    next_timer: slint::Timer,

    search_generation: Cell<u64>,
    search_timer: slint::Timer,
    preview_timer: slint::Timer,
    clock_timer: slint::Timer,
    aspect: Cell<i32>,
    /// Lo schermo intero l'ha acceso l'apertura del player (e la chiusura lo spegne).
    auto_fullscreen: Cell<bool>,
    /// Salva la dimensione del mini player poco dopo l'ultimo ridimensionamento.
    pip_save_timer: slint::Timer,
    pip_drag: Cell<Option<PipDrag>>,

    // guida
    /// Programmi attorno ad adesso, per «in onda / dopo».
    now_schedule: Rc<RefCell<Schedule>>,
    now_loaded_at: Cell<i64>,
    now_generation: Cell<u64>,
    /// La griglia della Guida e il giorno mostrato (0 oggi, -1 ieri…).
    guide: Rc<GuideModel>,
    guide_offset: Cell<i64>,
    day_generation: Cell<u64>,
    guide_timer: slint::Timer,
    /// Sorgenti (con i dati della guida) e aggiornamenti della guida in corso o falliti.
    sources: RefCell<Vec<SourceInfo>>,
    epg_status: RefCell<HashMap<i64, EpgUi>>,

    // elenchi che si aprono su quello che si sta guardando
    /// Ultimo film o serie riprodotto (tipo e chiave).
    last_media: RefCell<Option<(ItemKind, String)>>,
    /// Alla prossima lista di episodi, cursore sull'episodio corrente.
    reveal_episode: Cell<bool>,
    reveal_timer: slint::Timer,

    /// Versione nuova da proporre.
    update: RefCell<Option<Update>>,

    // controllo genitori
    /// Il PIN è stato inserito in questa sessione: le categorie bloccate si vedono.
    pin_unlocked: Cell<bool>,
    /// Richiesta del PIN aperta, e cosa fare quando è giusto.
    after_pin: Cell<Option<AfterPin>>,
}

/// Stato dell'aggiornamento della guida di una sorgente, finché non riesce.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EpgUi {
    Working,
    Failed(String),
}

/// Ora attuale (secondi Unix).
fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
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
        let channels = CatalogModel::new(request_image);
        channels.enable_custom();
        let movies = CatalogModel::new(request_image);
        let series = CatalogModel::new(request_image);
        let channel_categories = Rc::new(VecModel::default());
        let media_categories = Rc::new(VecModel::default());
        main.set_channels(ModelRc::from(Rc::clone(&channels)));
        main.set_categories(ModelRc::from(Rc::clone(&channel_categories)));
        main.set_media_items(ModelRc::from(Rc::clone(&movies)));
        main.set_media_categories(ModelRc::from(Rc::clone(&media_categories)));

        // «in onda / dopo» nelle righe dei canali, e la griglia che segue l'elenco dei canali
        let now_schedule = Rc::new(RefCell::new(Schedule::default()));
        {
            let schedule = Rc::clone(&now_schedule);
            channels.set_decorate(move |c: &ChannelRow, item: &mut ChannelItem| {
                guide::fill_now_next(item, &schedule.borrow(), &c.key, unix_now());
            });
        }
        let guide_model = GuideModel::new(Rc::clone(&channels));
        {
            let grid = Rc::downgrade(&guide_model);
            channels.set_on_change(move || {
                if let Some(grid) = grid.upgrade() {
                    grid.refresh();
                }
            });
        }
        main.set_guide_rows(ModelRc::from(Rc::clone(&guide_model)));
        main.set_guide_days(ModelRc::new(VecModel::from(guide::days())));

        let app = Rc::new(Self {
            main: main.as_weak(),
            pip: pip.as_weak(),
            service,
            router,
            settings: RefCell::new(Settings::defaults(language)),
            source_id: Cell::new(None),
            channels,
            movies,
            series,
            movie_rows: RefCell::new(Arc::new(Vec::new())),
            series_rows: RefCell::new(Arc::new(Vec::new())),
            channel_categories,
            media_categories,
            favorites: RefCell::new(Vec::new()),
            history: RefCell::new(Vec::new()),
            home_resume: RefCell::new(Vec::new()),
            home_favorites: RefCell::new(Vec::new()),
            home_images: RefCell::new(HashSet::new()),
            media_mode: Cell::new(0),
            detail: RefCell::new(None),
            series_cache: RefCell::new(HashMap::new()),
            started: Cell::new(false),
            current: Cell::new(None),
            now: RefCell::new(None),
            pending: RefCell::new(None),
            stream_token: Cell::new(0),
            progress: Cell::new((0.0, 0.0)),
            progress_ticks: Cell::new(0),
            progress_timer: slint::Timer::default(),
            next_episode: RefCell::new(None),
            next_countdown: Cell::new(-1),
            next_timer: slint::Timer::default(),
            search_generation: Cell::new(0),
            search_timer: slint::Timer::default(),
            preview_timer: slint::Timer::default(),
            clock_timer: slint::Timer::default(),
            aspect: Cell::new(0),
            auto_fullscreen: Cell::new(false),
            pip_save_timer: slint::Timer::default(),
            pip_drag: Cell::new(None),
            now_schedule,
            now_loaded_at: Cell::new(0),
            now_generation: Cell::new(0),
            guide: guide_model,
            guide_offset: Cell::new(0),
            day_generation: Cell::new(0),
            guide_timer: slint::Timer::default(),
            sources: RefCell::new(Vec::new()),
            epg_status: RefCell::new(HashMap::new()),
            last_media: RefCell::new(None),
            reveal_episode: Cell::new(false),
            reveal_timer: slint::Timer::default(),
            update: RefCell::new(None),
            pin_unlocked: Cell::new(false),
            after_pin: Cell::new(None),
        });
        APP.with(|a| *a.borrow_mut() = Rc::downgrade(&app));
        {
            let weak = Rc::downgrade(&app);
            app.router.set_on_finished(move || {
                if let Some(app) = weak.upgrade() {
                    app.on_finished();
                }
            });
        }
        app.apply_language(language);
        app.connect(main, pip);
        app.start_clock();
        app.start_guide_timer();
        app.service.send(Command::Init);
        app
    }

    fn main(&self) -> Option<MainWindow> {
        self.main.upgrade()
    }

    /// Ferma la riproduzione (alla chiusura) e salva il punto di visione.
    pub fn shutdown(&self) {
        self.save_progress(false);
        self.router.stop();
    }

    // ───────────── eventi del servizio ─────────────

    fn on_service_event(self: &Rc<Self>, event: Event) {
        match event {
            Event::Settings(values) => {
                let language = self.settings.borrow().language;
                self.apply_settings(Settings::from_pairs(&values, language));
            }
            Event::Sources(sources) => {
                *self.sources.borrow_mut() = sources;
                self.show_sources();
                self.update_guide_state();
            }
            Event::Epg { source_id, state } => self.on_epg(source_id, state),
            Event::Guide {
                source_id,
                generation,
                programmes,
                ..
            } => self.on_guide(source_id, generation, &programmes),
            Event::Catalog {
                source_id,
                channels,
                movies,
                series,
            } => self.on_catalog(source_id, channels, movies, series),
            Event::Favorites(favorites) => self.on_favorites(favorites),
            Event::History(history) => self.on_history(history),
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
            Event::SourceSaved { result, .. } => {
                let saved = result.is_ok();
                self.show_source_result(result, true);
                if let (true, Some(main)) = (saved, self.main()) {
                    // il modulo si svuota, password compresa
                    main.set_form_reset(main.get_form_reset().wrapping_add(1));
                }
            }
            Event::SearchResults {
                generation,
                channels,
                movies,
                series,
            } => {
                if generation == self.search_generation.get() {
                    self.channels.show_search(&channels);
                    self.movies.show_search(&movies);
                    self.series.show_search(&series);
                    self.after_filter_changed();
                    self.after_media_filter_changed();
                }
            }
            Event::Stream { token, result } => self.on_stream(token, result),
            Event::MovieDetail { id, result } => self.on_movie_detail(id, result),
            Event::SeriesDetail { id, result } => self.on_series_detail(id, result),
            Event::Image { url, poster, image } => self.on_image(url, poster, image),
            Event::Health(health) => self.on_health(health),
            Event::Update(update) => {
                if let Some(main) = self.main() {
                    main.set_update_version(update.version.as_str().into());
                }
                *self.update.borrow_mut() = Some(update);
            }
            Event::Error(message) => log::error!("{message}"),
        }
    }

    /// L'archivio era rovinato: riparato basta il log, altrimenti lo si dice (con la copia).
    fn on_health(&self, health: Health) {
        let Health::Damaged { backup, problem } = health else {
            return;
        };
        log::error!(
            "archivio danneggiato ({problem}), copia in {}",
            backup.display()
        );
        let text = match self.settings.borrow().language {
            Language::Italian => format!(
                "L'archivio di Sintonia è danneggiato e non si è potuto riparare da solo.                  Alcune sorgenti o impostazioni potrebbero mancare: se succede, aggiungile di                  nuovo.

Una copia dell'archivio com'era è in:
{}",
                backup.display()
            ),
            Language::English => format!(
                "Sintonia's database is damaged and could not be repaired automatically.                  Some sources or settings may be missing: if so, add them again.

                 A copy of the database as it was is in:
{}",
                backup.display()
            ),
        };
        let shown = slint::spawn_local(async move {
            rfd::AsyncMessageDialog::new()
                .set_title("Sintonia")
                .set_description(text)
                .set_level(rfd::MessageLevel::Warning)
                .show()
                .await;
        });
        if let Err(err) = shown {
            log::error!("{err}");
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
            main.set_live_preview(settings.live_preview);
            main.set_update_check(settings.update_check);
            main.set_playlist_interval(interval_index(settings.playlist_hours));
            main.set_epg_interval(interval_index(settings.epg_hours));
            main.set_user_agent(settings.user_agent.clone().unwrap_or_default().into());
        }
        *self.settings.borrow_mut() = settings;
        self.show_custom_name();
        self.apply_locks();
    }

    fn apply_language(&self, language: Language) {
        if let Err(err) = slint::select_bundled_translation(language.code()) {
            log::warn!("lingua {}: {err}", language.code());
        }
        if let Some(main) = self.main() {
            main.set_language(language.index());
        }
    }

    fn format_date(&self, secs: Option<i64>) -> String {
        let Some(date) = secs.and_then(|s| chrono::DateTime::from_timestamp(s, 0)) else {
            return String::new();
        };
        let pattern = match self.settings.borrow().language {
            Language::Italian => "%d/%m/%Y",
            Language::English => "%Y-%m-%d",
        };
        date.with_timezone(&chrono::Local)
            .format(pattern)
            .to_string()
    }

    /// Data e ora brevi («18/10 14:32»).
    fn format_date_time(&self, secs: Option<i64>) -> String {
        let Some(date) = secs.and_then(|s| chrono::DateTime::from_timestamp(s, 0)) else {
            return String::new();
        };
        let pattern = match self.settings.borrow().language {
            Language::Italian => "%d/%m %H:%M",
            Language::English => "%m-%d %H:%M",
        };
        date.with_timezone(&chrono::Local)
            .format(pattern)
            .to_string()
    }

    /// Stato della guida di una sorgente: 0 nessuna, 1 in aggiornamento, 2 pronta, 3 errore.
    fn epg_state_of(&self, source: &SourceInfo) -> (i32, String) {
        match self.epg_status.borrow().get(&source.id) {
            Some(EpgUi::Working) => (1, String::new()),
            Some(EpgUi::Failed(error)) => (3, error.clone()),
            None if source.epg_refreshed_at.is_some() => (2, String::new()),
            None => (0, String::new()),
        }
    }

    fn show_sources(&self) {
        let Some(main) = self.main() else { return };
        let sources = self.sources.borrow();
        let items: Vec<SourceItem> = sources
            .iter()
            .map(|s| SourceItem {
                id: i32::try_from(s.id).unwrap_or(-1),
                name: s.name.as_str().into(),
                detail: s.display.as_str().into(),
                kind: match s.kind {
                    SourceKind::M3uUrl => 0,
                    SourceKind::M3uFile => 1,
                    SourceKind::Xtream => 2,
                },
                count: i32::try_from(s.channel_count).unwrap_or(i32::MAX),
                movies: i32::try_from(s.movie_count).unwrap_or(i32::MAX),
                series: i32::try_from(s.series_count).unwrap_or(i32::MAX),
                expires: self.format_date(s.expires).into(),
                active: s.active,
                epg_state: self.epg_state_of(s).0,
                epg_matched: i32::try_from(s.epg_matched).unwrap_or(i32::MAX),
                epg_updated: self.format_date_time(s.epg_refreshed_at).into(),
                epg_error: self.epg_state_of(s).1.into(),
            })
            .collect();
        main.set_no_source(items.is_empty());
        main.set_sources(ModelRc::new(VecModel::from(items)));
    }

    fn show_source_result(&self, result: Result<SourceReport, SourceError>, saved: bool) {
        let Some(main) = self.main() else { return };
        let status = match result {
            Ok(report) => {
                let mut status = SourceStatus {
                    state: if saved { 3 } else { 2 },
                    channels: count_i32(report.channels),
                    groups: count_i32(report.groups),
                    skipped: count_i32(report.skipped),
                    movies: count_i32(report.movies),
                    series: count_i32(report.series),
                    ..Default::default()
                };
                if let Some(account) = &report.account {
                    status.account = if account.is_active() { 1 } else { 2 };
                    status.account_status = account.status.as_str().into();
                    status.expires = self.format_date(account.expires).into();
                    status.connections = account
                        .max_connections
                        .map_or(0, |c| i32::try_from(c).unwrap_or(i32::MAX));
                }
                status
            }
            Err(err) => source_error_status(&err),
        };
        main.set_source_status(status);
    }

    fn on_catalog(
        self: &Rc<Self>,
        source_id: Option<i64>,
        channels: Arc<Vec<ChannelRow>>,
        movies: Arc<Vec<MovieRow>>,
        series: Arc<Vec<SeriesRow>>,
    ) {
        let changed_source = self.source_id.replace(source_id) != source_id;
        // dopo un aggiornamento gli identificativi possono cambiare, le chiavi no
        let current_key = if changed_source {
            None
        } else {
            self.current
                .get()
                .and_then(|id| self.channels.with_entry(id, |c| c.key.clone()))
        };
        if changed_source {
            if self.router.is_active() || self.pending.borrow().is_some() {
                self.stop_playback();
            }
            self.series_cache.borrow_mut().clear();
            *self.detail.borrow_mut() = None;
            if let Some(main) = self.main() {
                main.set_media_detail(0);
            }
        }

        // i blocchi sono della sorgente: prima delle voci, così «Tutti» nasce già filtrato
        self.set_model_locks();
        self.channels.set_entries(channels);
        let movie_entries: Vec<PosterEntry> = movies.iter().map(PosterEntry::movie).collect();
        let series_entries: Vec<PosterEntry> = series.iter().map(PosterEntry::series).collect();
        self.movies.set_entries(Arc::new(movie_entries));
        self.series.set_entries(Arc::new(series_entries));
        *self.movie_rows.borrow_mut() = movies;
        *self.series_rows.borrow_mut() = series;

        if let Some(now) = self.now.borrow_mut().as_mut() {
            if let Some(id) = self.channels.id_of_key(&now.key) {
                if matches!(now.item, Playable::Channel(_)) {
                    now.item = Playable::Channel(id);
                }
            }
        }

        let last_key = self.settings.borrow().last_channel.clone();
        let current = current_key
            .and_then(|k| self.channels.id_of_key(&k))
            .or_else(|| last_key.and_then(|k| self.channels.id_of_key(&k)))
            .or_else(|| self.channels.first_id());
        if changed_source {
            *self.now_schedule.borrow_mut() = Schedule::default();
            let (_, date) = guide::day_start(self.guide_offset.get());
            self.guide.set_day(date, Schedule::default());
        }
        self.after_channels_changed(current);
        self.after_media_changed();
        self.refresh_home();
        self.refresh_lock_lists();
        self.update_guide_state();
        self.request_now();
        self.request_day();

        // all'avvio riprende l'ultimo canale (l'anteprima della Home è «in onda»)
        if !self.started.replace(true) {
            if let Some(id) = current {
                self.play_channel(id);
            }
        }
    }

    fn after_channels_changed(&self, current: Option<i64>) {
        let Some(main) = self.main() else { return };
        let categories = self.channels.categories();
        // nella Home: le categorie della sorgente, senza «Tutti», «Preferiti» e la personalizzata
        let first_group = self.channels.first_group();
        let top: Vec<CategoryItem> = categories
            .iter()
            .skip(first_group)
            .take(HOME_CATEGORIES)
            .cloned()
            .collect();
        self.channel_categories.set_vec(categories);
        main.set_top_categories(ModelRc::new(VecModel::from(top)));
        main.set_top_offset(count_i32(first_group));
        if !main.get_search().is_empty() {
            main.set_search(SharedString::new());
            self.search_generation.set(self.search_generation.get() + 1);
        }
        if !main.get_on_demand() {
            self.set_current(current);
        } else {
            self.current.set(current);
        }
        self.after_filter_changed();
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

    fn media_model(&self) -> &Rc<CatalogModel<PosterEntry>> {
        if self.media_mode.get() == 1 {
            &self.series
        } else {
            &self.movies
        }
    }

    fn after_media_changed(&self) {
        let Some(main) = self.main() else { return };
        main.set_media_available(!self.movies.is_empty() || !self.series.is_empty());
        // se c'è un solo tipo di contenuto si mostra quello
        let mode = match (self.movies.is_empty(), self.series.is_empty()) {
            (true, false) => 1,
            (false, true) => 0,
            _ => self.media_mode.get(),
        };
        self.show_media_mode(mode);
    }

    fn show_media_mode(&self, mode: i32) {
        let Some(main) = self.main() else { return };
        self.media_mode.set(mode);
        let model = self.media_model();
        main.set_media_mode(mode);
        main.set_media_items(ModelRc::from(Rc::clone(model)));
        self.media_categories.set_vec(model.categories());
        self.after_media_filter_changed();
    }

    fn after_media_filter_changed(&self) {
        let Some(main) = self.main() else { return };
        main.set_media_category(i32::try_from(self.media_model().category()).unwrap_or(0));
        main.set_media_cursor(0);
    }

    fn on_favorites(&self, favorites: Vec<(ItemKind, String)>) {
        let mut live = HashSet::new();
        let mut movies = HashSet::new();
        let mut series = HashSet::new();
        for (kind, key) in &favorites {
            match kind {
                ItemKind::Live => live.insert(key.clone()),
                ItemKind::Movie => movies.insert(key.clone()),
                ItemKind::Series => series.insert(key.clone()),
            };
        }
        self.channels.set_favorites(live);
        self.movies.set_favorites(movies);
        self.series.set_favorites(series);
        *self.favorites.borrow_mut() = favorites;

        // solo il conteggio dei preferiti cambia: l'elenco delle categorie resta dov'è
        if let Some(fav) = self.channels.categories().get(FAVORITES) {
            self.channel_categories.set_row_data(FAVORITES, fav.clone());
        }
        if let Some(fav) = self.media_model().categories().get(FAVORITES) {
            self.media_categories.set_row_data(FAVORITES, fav.clone());
        }
        if let Some(main) = self.main() {
            let rows = self.channels.row_count();
            if usize::try_from(main.get_cursor()).is_ok_and(|c| c >= rows) {
                main.set_cursor(count_i32(rows.saturating_sub(1)));
            }
            let rows = self.media_model().row_count();
            if usize::try_from(main.get_media_cursor()).is_ok_and(|c| c >= rows) {
                main.set_media_cursor(count_i32(rows.saturating_sub(1)));
            }
        }
        self.refresh_detail_info();
        self.refresh_home();
    }

    fn on_history(&self, history: Vec<HistoryEntry>) {
        let progress: HashMap<String, f32> = history
            .iter()
            .filter(|e| e.kind == Some(ItemKind::Movie) && e.duration > 0.0)
            .map(|e| {
                (
                    e.key.clone(),
                    (e.position / e.duration).clamp(0.0, 1.0) as f32,
                )
            })
            .collect();
        self.movies.set_progress(progress);
        *self.history.borrow_mut() = history;
        self.refresh_home();
        self.refresh_detail_info();
        self.refresh_episodes();
    }

    fn history_entry(&self, kind: ItemKind, key: &str) -> Option<HistoryEntry> {
        self.history
            .borrow()
            .iter()
            .find(|e| e.kind == Some(kind) && e.key == key)
            .cloned()
    }

    fn on_image(&self, url: String, poster: bool, image: Option<LogoImage>) {
        if poster {
            let image = image.map(images::poster);
            self.movies.image_ready(url.clone(), image.clone());
            self.series.image_ready(url.clone(), image);
            let is_cover = self
                .detail
                .borrow()
                .as_ref()
                .is_some_and(|d| d.cover.as_deref() == Some(url.as_str()));
            if is_cover {
                self.refresh_detail_info();
            }
        } else {
            self.channels
                .image_ready(url.clone(), image.map(images::logo));
            // il logo del canale in onda (Home e mini player)
            let current_logo = self.current.get().and_then(|id| {
                self.channels
                    .with_entry(id, |c| c.logo.as_deref() == Some(url.as_str()))
            });
            if current_logo == Some(true) && self.main().is_some_and(|m| !m.get_on_demand()) {
                self.set_current(self.current.get());
            }
        }
        if self.home_images.borrow().contains(&url) {
            self.refresh_home();
        }
    }

    // ───────────── Home ─────────────

    fn refresh_home(&self) {
        let Some(main) = self.main() else { return };
        let mut images = HashSet::new();
        let mut resume_entries = Vec::new();
        let mut resume_items = Vec::new();
        let mut live = 0;
        for entry in self.history.borrow().iter() {
            if resume_items.len() >= HOME_ROW_ITEMS {
                break;
            }
            let Some(kind) = entry.kind else { continue };
            if kind == ItemKind::Live && live >= HOME_LIVE_ITEMS {
                continue;
            }
            let Some(item) = self.home_item(kind, &entry.key, Some(entry), &mut images) else {
                continue;
            };
            if kind == ItemKind::Live {
                live += 1;
            }
            resume_items.push(item);
            resume_entries.push(entry.clone());
        }
        let mut favorite_keys = Vec::new();
        let mut favorite_items = Vec::new();
        for (kind, key) in self.favorites.borrow().iter() {
            if favorite_items.len() >= HOME_ROW_ITEMS {
                break;
            }
            if let Some(item) = self.home_item(*kind, key, None, &mut images) {
                favorite_items.push(item);
                favorite_keys.push((*kind, key.clone()));
            }
        }
        main.set_resume(ModelRc::new(VecModel::from(resume_items)));
        main.set_home_favorites(ModelRc::new(VecModel::from(favorite_items)));
        *self.home_resume.borrow_mut() = resume_entries;
        *self.home_favorites.borrow_mut() = favorite_keys;
        *self.home_images.borrow_mut() = images;
    }

    /// Una scheda della Home; `None` se il contenuto non è (più) nella sorgente.
    fn home_item(
        &self,
        kind: ItemKind,
        key: &str,
        entry: Option<&HistoryEntry>,
        images: &mut HashSet<String>,
    ) -> Option<HomeItem> {
        if kind == ItemKind::Live {
            let id = self.channels.id_of_key(key)?;
            let (name, group, number, logo) = self.channels.with_entry(id, |c| {
                (c.name.clone(), c.group.clone(), c.number, c.logo.clone())
            })?;
            let image = logo.and_then(|url| {
                let image = self.channels.image(&url);
                images.insert(url);
                image
            });
            return Some(HomeItem {
                tint: tint_for(&name),
                title: name.into(),
                subtitle: group.into(),
                num: format_number(number).into(),
                has_image: image.is_some(),
                image: image.unwrap_or_default(),
                progress: 0.0,
                kind: 0,
            });
        }
        let model = if kind == ItemKind::Movie {
            &self.movies
        } else {
            &self.series
        };
        let id = model.id_of_key(key)?;
        let (title, meta, cover) =
            model.with_entry(id, |p| (p.title.clone(), p.meta.clone(), p.cover.clone()))?;
        let image = cover.and_then(|url| {
            let image = model.image(&url);
            images.insert(url);
            image
        });
        let subtitle = entry
            .map(|e| e.subtitle.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or(meta);
        let progress = entry
            .filter(|e| e.duration > 0.0)
            .map_or(0.0, |e| (e.position / e.duration).clamp(0.0, 1.0) as f32);
        Some(HomeItem {
            tint: tint_for(&title),
            title: title.into(),
            subtitle: subtitle.into(),
            num: SharedString::new(),
            has_image: image.is_some(),
            image: image.unwrap_or_default(),
            progress,
            kind: if kind == ItemKind::Movie { 1 } else { 2 },
        })
    }

    fn open_resume(&self, index: usize) {
        let Some(entry) = self.home_resume.borrow().get(index).cloned() else {
            return;
        };
        let start = resume_start(entry.position, entry.duration);
        match entry.kind {
            Some(ItemKind::Live) => {
                if let Some(id) = self.channels.id_of_key(&entry.key) {
                    self.play_channel(id);
                    self.show_player();
                }
            }
            Some(ItemKind::Movie) => {
                if let Some(id) = self.movies.id_of_key(&entry.key) {
                    self.play_movie(id, start);
                    self.show_player();
                }
            }
            Some(ItemKind::Series) => {
                if let (Some(id), Some(episode)) =
                    (self.series.id_of_key(&entry.key), entry.episode)
                {
                    self.play_episode(id, episode, start);
                    self.show_player();
                }
            }
            None => {}
        }
    }

    fn open_favorite(&self, index: usize) {
        let Some((kind, key)) = self.home_favorites.borrow().get(index).cloned() else {
            return;
        };
        match kind {
            ItemKind::Live => {
                if let Some(id) = self.channels.id_of_key(&key) {
                    self.play_channel(id);
                    self.show_player();
                }
            }
            ItemKind::Movie | ItemKind::Series => {
                let Some(main) = self.main() else { return };
                let mode = if kind == ItemKind::Movie { 0 } else { 1 };
                if mode != self.media_mode.get() {
                    self.show_media_mode(mode);
                }
                if let Some(id) = self.media_model().id_of_key(&key) {
                    self.open_detail(kind, id);
                    main.set_page(3);
                }
            }
        }
    }

    /// Apre il player a tutto schermo nella finestra principale.
    fn show_player(&self) {
        let Some(main) = self.main() else { return };
        self.router.move_to_main();
        main.set_playing(true);
    }

    /// Chiude il player (dopo la fine di un film, o «Annulla» sul prossimo episodio) e torna
    /// alla pagina da cui era stato aperto.
    fn leave_player(&self) {
        let Some(main) = self.main() else { return };
        if main.get_playing() {
            main.set_playing(false);
        } else if self.pip.upgrade().is_some_and(|p| p.window().is_visible()) {
            self.stop_playback();
            self.router.move_to_main();
        }
    }

    // ───────────── Film e serie ─────────────

    fn open_detail(&self, kind: ItemKind, id: i64) {
        let Some(main) = self.main() else { return };
        let model = if kind == ItemKind::Movie {
            &self.movies
        } else {
            &self.series
        };
        let Some(entry) = model.with_entry(id, PosterEntry::clone) else {
            return;
        };
        let rating = format_rating(entry.rating);
        let meta = [entry.meta.as_str(), rating_label(&rating).as_str()]
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let mut detail = Detail {
            kind,
            id,
            key: entry.key,
            title: entry.title,
            cover: entry.cover,
            meta,
            plot: String::new(),
            credits: String::new(),
            loading: true,
            seasons: Vec::new(),
            season: 0,
        };
        if kind == ItemKind::Series {
            detail.plot = self
                .series_rows
                .borrow()
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.plot.clone())
                .unwrap_or_default();
            let cached = self.series_cache.borrow().get(&id).cloned();
            match cached {
                Some(cached) => self.apply_series_detail(&mut detail, &cached),
                None => self.service.send(Command::SeriesDetail(id)),
            }
        } else {
            self.service.send(Command::MovieDetail(id));
        }
        *self.detail.borrow_mut() = Some(detail);
        main.set_media_episode_cursor(0);
        self.reveal_episode.set(true);
        main.set_media_detail(if kind == ItemKind::Movie { 1 } else { 2 });
        self.refresh_detail_info();
        self.refresh_episodes();
    }

    fn on_movie_detail(&self, id: i64, result: Result<MovieDetail, String>) {
        let Some(main) = self.main() else { return };
        {
            let mut detail = self.detail.borrow_mut();
            let Some(d) = detail
                .as_mut()
                .filter(|d| d.kind == ItemKind::Movie && d.id == id)
            else {
                return;
            };
            d.loading = false;
            match result {
                Ok(movie) => {
                    let texts = main.global::<Texts>();
                    let year = movie
                        .release_date
                        .as_deref()
                        .and_then(|d| d.get(..4))
                        .map(str::to_owned)
                        .or_else(|| {
                            self.movies
                                .with_entry(id, |p| p.meta.clone())
                                .filter(|m| !m.is_empty())
                        });
                    let minutes = movie
                        .duration_secs
                        .filter(|s| *s > 0)
                        .map(|s| texts.invoke_minutes(count_i32((s as usize).div_ceil(60))));
                    let rating = format_rating(
                        movie
                            .rating
                            .or_else(|| self.movies.with_entry(id, |p| p.rating).flatten()),
                    );
                    d.meta = [
                        year,
                        movie.genre.clone(),
                        minutes.map(|m| m.to_string()),
                        Some(rating_label(&rating)),
                    ]
                    .into_iter()
                    .flatten()
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                    d.plot = movie.plot.unwrap_or_default();
                    d.credits = [
                        movie
                            .director
                            .filter(|t| !t.trim().is_empty())
                            .map(|t| texts.invoke_director(t.trim().into()).to_string()),
                        movie.cast.filter(|t| !t.trim().is_empty()),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n");
                }
                Err(err) => log::warn!("dettagli del film: {err}"),
            }
        }
        self.refresh_detail_info();
    }

    fn on_series_detail(&self, id: i64, result: Result<SeriesDetail, String>) {
        match result {
            Ok(detail) => {
                self.series_cache.borrow_mut().insert(id, Rc::new(detail));
            }
            Err(err) => log::warn!("episodi della serie: {err}"),
        }
        let cached = self.series_cache.borrow().get(&id).cloned();
        {
            let mut detail = self.detail.borrow_mut();
            let Some(d) = detail
                .as_mut()
                .filter(|d| d.kind == ItemKind::Series && d.id == id)
            else {
                return;
            };
            d.loading = false;
            if let Some(cached) = cached {
                self.apply_series_detail(d, &cached);
            }
        }
        self.refresh_detail_info();
        self.refresh_episodes();
    }

    /// Stagioni ed episodi nella scheda; si apre sulla stagione dell'episodio da riprendere.
    fn apply_series_detail(&self, detail: &mut Detail, series: &SeriesDetail) {
        detail.loading = false;
        if detail.plot.is_empty() {
            detail.plot = series.plot.clone().unwrap_or_default();
        }
        let mut seasons: Vec<u32> = series.episodes.iter().map(|e| e.season).collect();
        seasons.dedup();
        let resume_season = self
            .history_entry(ItemKind::Series, &detail.key)
            .and_then(|e| e.episode)
            .map(|e| e.season);
        detail.season = resume_season
            .and_then(|s| seasons.iter().position(|x| *x == s))
            .unwrap_or(0);
        detail.seasons = seasons;
    }

    fn refresh_detail_info(&self) {
        let Some(main) = self.main() else { return };
        let detail = self.detail.borrow();
        let Some(d) = detail.as_ref() else { return };
        let model = if d.kind == ItemKind::Movie {
            &self.movies
        } else {
            &self.series
        };
        let cover = d.cover.as_deref().and_then(|url| model.image(url));
        let history = self.history_entry(d.kind, &d.key);
        let (resume_secs, resume_label) = match (d.kind, history) {
            (ItemKind::Movie, Some(e)) => (
                resume_start(e.position, e.duration).map_or(0, |s| i32::try_from(s).unwrap_or(0)),
                String::new(),
            ),
            (ItemKind::Series, Some(e)) => (
                0,
                e.episode
                    .map(|ep| episode_label(ep.season, ep.number))
                    .unwrap_or_default(),
            ),
            _ => (0, String::new()),
        };
        let texts = main.global::<Texts>();
        let seasons: Vec<SharedString> = d
            .seasons
            .iter()
            .map(|s| texts.invoke_season(i32::try_from(*s).unwrap_or(0)))
            .collect();
        main.set_media_info(DetailInfo {
            title: d.title.as_str().into(),
            meta: d.meta.as_str().into(),
            plot: d.plot.as_str().into(),
            credits: d.credits.as_str().into(),
            has_cover: cover.is_some(),
            cover: cover.unwrap_or_default(),
            tint: tint_for(&d.title),
            fav: model.is_favorite(&d.key),
            loading: d.loading,
            resume_secs,
            resume_label: resume_label.into(),
        });
        main.set_media_seasons(ModelRc::new(VecModel::from(seasons)));
        main.set_media_season(count_i32(d.season));
    }

    /// Episodi della stagione mostrata.
    fn season_episodes(&self) -> Vec<Episode> {
        let detail = self.detail.borrow();
        let Some(d) = detail.as_ref().filter(|d| d.kind == ItemKind::Series) else {
            return Vec::new();
        };
        let Some(season) = d.seasons.get(d.season) else {
            return Vec::new();
        };
        let cache = self.series_cache.borrow();
        cache.get(&d.id).map_or_else(Vec::new, |s| {
            s.episodes
                .iter()
                .filter(|e| e.season == *season)
                .cloned()
                .collect()
        })
    }

    fn refresh_episodes(&self) {
        let Some(main) = self.main() else { return };
        let key = self.detail.borrow().as_ref().map(|d| d.key.clone());
        let history = key.and_then(|k| self.history_entry(ItemKind::Series, &k));
        let texts = main.global::<Texts>();
        let items: Vec<EpisodeItem> = self
            .season_episodes()
            .iter()
            .map(|e| {
                let seen = history
                    .as_ref()
                    .filter(|h| h.episode.as_ref().is_some_and(|ep| ep.id == e.id));
                EpisodeItem {
                    id: i32::try_from(e.id).unwrap_or(-1),
                    season: i32::try_from(e.season).unwrap_or(0),
                    number: i32::try_from(e.number).unwrap_or(0),
                    title: if e.title.trim().is_empty() {
                        format!("E{}", e.number).into()
                    } else {
                        e.title.as_str().into()
                    },
                    duration: e
                        .duration_secs
                        .filter(|s| *s > 0)
                        .map(|s| texts.invoke_minutes(count_i32((s as usize).div_ceil(60))))
                        .unwrap_or_default(),
                    progress: seen
                        .filter(|h| h.duration > 0.0)
                        .map_or(0.0, |h| (h.position / h.duration).clamp(0.0, 1.0) as f32),
                    current: seen.is_some(),
                }
            })
            .collect();
        // all'apertura della scheda: cursore sull'episodio che si sta guardando (o l'ultimo visto)
        let current = items.iter().position(|e| e.current);
        main.set_media_episodes(ModelRc::new(VecModel::from(items)));
        if let Some(index) = current {
            if self.reveal_episode.replace(false) {
                main.set_media_episode_cursor(count_i32(index));
                self.reveal_later();
            }
        }
    }

    /// Fra un attimo (quando la pagina ha le sue misure) gli elenchi mostrano al centro il
    /// cursore, cioè quello che si sta guardando.
    fn reveal_later(&self) {
        let main = self.main.clone();
        self.reveal_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(80),
            move || {
                if let Some(main) = main.upgrade() {
                    main.set_reveal_tick(main.get_reveal_tick().wrapping_add(1));
                    if main.get_page() == 2 {
                        main.set_guide_scroll_tick(main.get_guide_scroll_tick().wrapping_add(1));
                    }
                }
            },
        );
    }

    /// Una pagina si apre: il cursore va su quello che si sta guardando (se è nell'elenco).
    fn on_page_changed(&self, page: i32) {
        let Some(main) = self.main() else { return };
        match page {
            1 => {
                if let Some(row) = self.current.get().and_then(|id| self.channels.row_of(id)) {
                    main.set_cursor(count_i32(row));
                }
            }
            3 => self.point_at_last_media(),
            _ => {}
        }
        self.reveal_later();
    }

    /// In Film e serie: la scheda giusta (film o serie) e il cursore sull'ultimo riprodotto.
    fn point_at_last_media(&self) {
        let Some(main) = self.main() else { return };
        if self.detail.borrow().is_some() {
            return;
        }
        let Some((kind, key)) = self.last_media.borrow().clone() else {
            return;
        };
        let mode = if kind == ItemKind::Movie { 0 } else { 1 };
        if mode != self.media_mode.get() {
            self.show_media_mode(mode);
        }
        let model = self.media_model();
        if let Some(row) = model.id_of_key(&key).and_then(|id| model.row_of(id)) {
            main.set_media_cursor(count_i32(row));
        }
    }

    fn close_detail(&self) {
        *self.detail.borrow_mut() = None;
        if let Some(main) = self.main() {
            main.set_media_detail(0);
        }
    }

    fn toggle_favorite(&self, kind: ItemKind, key: String) {
        let model_has = match kind {
            ItemKind::Live => self.channels.is_favorite(&key),
            ItemKind::Movie => self.movies.is_favorite(&key),
            ItemKind::Series => self.series.is_favorite(&key),
        };
        self.service.send(Command::SetFavorite {
            kind,
            key,
            favorite: !model_has,
        });
    }

    // ───────────── riproduzione ─────────────

    fn set_current(&self, id: Option<i64>) {
        self.current.set(id);
        self.guide.set_now(unix_now(), id);
        let item = id.and_then(|id| self.channels.item(id));
        if let Some(main) = self.main() {
            main.set_has_current(item.is_some());
        }
        self.show_item(item.unwrap_or_default());
    }

    /// Mostra una voce come «in onda» (Home, player, mini player).
    fn show_item(&self, item: ChannelItem) {
        if let Some(pip) = self.pip.upgrade() {
            pip.set_num(item.num.clone());
            pip.set_name(item.name.clone());
            pip.set_tint(item.tint);
        }
        if let Some(main) = self.main() {
            main.set_current(item);
        }
    }

    /// Chiede al servizio l'indirizzo del canale; si apre quando arriva (evento `Stream`).
    fn play_channel(&self, id: i64) {
        self.preview_timer.stop();
        self.set_current(Some(id));
        let same = |item: &Playable| *item == Playable::Channel(id);
        let playing =
            self.router.is_active() && self.now.borrow().as_ref().is_some_and(|n| same(&n.item));
        let requested = self
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|p| same(&p.now.item));
        if playing || requested {
            return;
        }
        let now = self.channels.with_entry(id, |c| NowPlaying {
            item: Playable::Channel(id),
            kind: ItemKind::Live,
            key: c.key.clone(),
            title: c.name.clone(),
            subtitle: c.group.clone(),
            cover: c.logo.clone(),
        });
        if let Some(now) = now {
            self.request(now, None);
        }
    }

    fn play_movie(&self, id: i64, start: Option<u32>) {
        let now = self.movies.with_entry(id, |m| NowPlaying {
            item: Playable::Movie(id),
            kind: ItemKind::Movie,
            key: m.key.clone(),
            title: m.title.clone(),
            subtitle: m.meta.clone(),
            cover: m.cover.clone(),
        });
        if let Some(now) = now {
            self.request(now, start);
        }
    }

    fn play_episode(&self, series_id: i64, episode: EpisodeRef, start: Option<u32>) {
        let now = self.series.with_entry(series_id, |s| NowPlaying {
            subtitle: episode_label(episode.season, episode.number),
            item: Playable::Episode {
                series_id,
                episode: episode.clone(),
            },
            kind: ItemKind::Series,
            key: s.key.clone(),
            title: s.title.clone(),
            cover: s.cover.clone(),
        });
        let Some(now) = now else { return };
        // servono gli episodi per sapere qual è il prossimo
        if !self.series_cache.borrow().contains_key(&series_id) {
            self.service.send(Command::SeriesDetail(series_id));
        }
        self.request(now, start);
    }

    fn request(&self, now: NowPlaying, start: Option<u32>) {
        self.preview_timer.stop();
        self.stop_countdown();
        if self
            .now
            .borrow()
            .as_ref()
            .is_some_and(NowPlaying::on_demand)
        {
            self.save_progress(true);
        }
        self.progress_timer.stop();
        let token = self.stream_token.get() + 1;
        self.stream_token.set(token);
        if let Some(main) = self.main() {
            let on_demand = now.on_demand();
            main.set_on_demand(on_demand);
            main.set_position(start.map_or(0, |s| i32::try_from(s).unwrap_or(0)));
            main.set_duration(0);
            main.set_paused(false);
            if on_demand {
                self.show_item(on_demand_item(&now));
                main.set_has_current(true);
            }
        }
        self.service.send(Command::Resolve {
            item: now.item.clone(),
            token,
        });
        *self.pending.borrow_mut() = Some(Pending { token, now, start });
    }

    fn on_stream(self: &Rc<Self>, token: u64, result: Result<ResolvedStream, String>) {
        let pending = {
            let mut pending = self.pending.borrow_mut();
            if pending.as_ref().is_some_and(|p| p.token == token) {
                pending.take()
            } else {
                None
            }
        };
        let Some(Pending { now, start, .. }) = pending else {
            return;
        };
        match result {
            Ok(ResolvedStream { stream, on_demand }) => {
                self.router.play(stream, on_demand, start);
                self.progress.set((start.map_or(0.0, f64::from), 0.0));
                self.progress_ticks.set(0);
                if now.kind == ItemKind::Live {
                    self.save_setting("last_channel", &now.key);
                    self.service.send(Command::SaveHistory {
                        entry: now.history(0.0, 0.0),
                        notify: true,
                    });
                } else {
                    self.start_progress_timer();
                    *self.last_media.borrow_mut() = Some((now.kind, now.key.clone()));
                }
                *self.now.borrow_mut() = Some(now);
            }
            Err(err) => log::error!("contenuto non apribile: {err}"),
        }
    }

    fn start_progress_timer(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.progress_timer
            .start(slint::TimerMode::Repeated, PROGRESS_TICK, move || {
                if let Some(app) = weak.upgrade() {
                    app.progress_tick();
                }
            });
    }

    fn progress_tick(&self) {
        if !self.router.is_active() {
            return;
        }
        let Some(position) = self.router.position() else {
            return;
        };
        let player = self.router.player();
        let duration = player.duration().unwrap_or(0.0);
        let paused = player.is_paused();
        self.progress.set((position, duration));
        if let Some(main) = self.main() {
            main.set_position(secs_i32(position));
            main.set_duration(secs_i32(duration));
            main.set_paused(paused);
        }
        let ticks = self.progress_ticks.get().wrapping_add(1);
        self.progress_ticks.set(ticks);
        if ticks.is_multiple_of(SAVE_EVERY_TICKS) && !paused {
            self.save_progress(false);
        }
    }

    /// Salva il punto di visione del film o dell'episodio in corso.
    fn save_progress(&self, notify: bool) {
        let Some(now) = self.now.borrow().clone().filter(NowPlaying::on_demand) else {
            return;
        };
        let (position, duration) = self.progress.get();
        if position < 1.0 {
            return;
        }
        if is_finished(position, duration) {
            self.finish_history(&now);
        } else {
            self.service.send(Command::SaveHistory {
                entry: now.history(position, duration),
                notify,
            });
        }
    }

    /// Un contenuto visto fino in fondo: un film esce da «Riprendi», una serie passa
    /// all'episodio successivo (dall'inizio).
    fn finish_history(&self, now: &NowPlaying) {
        match self.next_episode_of(now) {
            Some(next) => self.service.send(Command::SaveHistory {
                entry: next.history(0.0, 0.0),
                notify: true,
            }),
            None => self.service.send(Command::RemoveHistory {
                kind: now.kind,
                key: now.key.clone(),
            }),
        }
    }

    fn next_episode_of(&self, now: &NowPlaying) -> Option<NowPlaying> {
        let Playable::Episode { series_id, episode } = &now.item else {
            return None;
        };
        let cache = self.series_cache.borrow();
        let episodes = &cache.get(series_id)?.episodes;
        let index = episodes.iter().position(|e| e.id == episode.id)?;
        let next = episodes.get(index + 1)?;
        Some(NowPlaying {
            item: Playable::Episode {
                series_id: *series_id,
                episode: episode_ref(next),
            },
            subtitle: episode_label(next.season, next.number),
            ..now.clone()
        })
    }

    /// Un film o un episodio è arrivato alla fine.
    fn on_finished(self: &Rc<Self>) {
        self.progress_timer.stop();
        let Some(now) = self.now.borrow_mut().take() else {
            return;
        };
        self.finish_history(&now);
        let Some(next) = self.next_episode_of(&now) else {
            self.leave_player();
            return;
        };
        let Some(main) = self.main() else { return };
        let label = match self.season_title(&next) {
            Some(title) => format!("{} · {title}", next.subtitle),
            None => next.subtitle.clone(),
        };
        main.set_next_label(label.into());
        main.set_next_countdown(NEXT_EPISODE_SECS);
        self.next_countdown.set(NEXT_EPISODE_SECS);
        *self.next_episode.borrow_mut() = Some(next);
        let weak = Rc::downgrade(self);
        self.next_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                let Some(app) = weak.upgrade() else { return };
                let left = app.next_countdown.get() - 1;
                app.next_countdown.set(left);
                if let Some(main) = app.main() {
                    main.set_next_countdown(left.max(0));
                }
                if left <= 0 {
                    app.play_next();
                }
            },
        );
    }

    /// Titolo dell'episodio (se diverso dal solo numero).
    fn season_title(&self, now: &NowPlaying) -> Option<String> {
        let Playable::Episode { series_id, episode } = &now.item else {
            return None;
        };
        let cache = self.series_cache.borrow();
        cache
            .get(series_id)?
            .episodes
            .iter()
            .find(|e| e.id == episode.id)
            .map(|e| e.title.trim().to_owned())
            .filter(|t| !t.is_empty())
    }

    fn play_next(&self) {
        self.next_timer.stop();
        let next = self.next_episode.borrow_mut().take();
        if let Some(main) = self.main() {
            main.set_next_countdown(-1);
        }
        if let Some(next) = next {
            self.request(next, None);
        }
    }

    fn stop_countdown(&self) {
        self.next_timer.stop();
        self.next_episode.borrow_mut().take();
        if let Some(main) = self.main() {
            main.set_next_countdown(-1);
        }
    }

    /// Ferma tutto e torna a mostrare il canale «in onda».
    fn stop_playback(&self) {
        self.stop_countdown();
        self.save_progress(true);
        self.progress_timer.stop();
        self.pending.borrow_mut().take();
        self.now.borrow_mut().take();
        self.router.stop();
        if let Some(main) = self.main() {
            main.set_on_demand(false);
        }
        self.set_current(self.current.get());
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

    // ───────────── guida ─────────────

    /// Ricarica i programmi attorno ad adesso.
    fn request_now(&self) {
        let generation = self.now_generation.get() + 1;
        self.now_generation.set(generation);
        let now = unix_now();
        self.now_loaded_at.set(now);
        self.service.send(Command::LoadGuide {
            from: now - NOW_WINDOW_PAST,
            to: now + NOW_WINDOW_FUTURE,
            generation,
        });
    }

    /// Carica i programmi del giorno mostrato nella Guida.
    fn request_day(&self) {
        let generation = self.day_generation.get() + 1;
        self.day_generation.set(generation);
        let (start, _) = guide::day_start(self.guide_offset.get());
        // un'ora in più: il giorno del cambio d'ora ne ha 25
        self.service.send(Command::LoadGuide {
            from: start,
            to: start + 25 * 3600,
            generation,
        });
    }

    fn on_guide(&self, source_id: Option<i64>, generation: u64, programmes: &[GuideProgramme]) {
        if source_id != self.source_id.get() {
            return;
        }
        if generation == self.now_generation.get() {
            *self.now_schedule.borrow_mut() = Schedule::new(programmes);
            self.channels.refresh();
            self.refresh_on_air();
        } else if generation == self.day_generation.get() {
            let (_, date) = guide::day_start(self.guide_offset.get());
            self.guide.set_day(date, Schedule::new(programmes));
        }
    }

    fn on_epg(&self, source_id: i64, state: EpgState) {
        let reload = match state {
            EpgState::Working => {
                self.epg_status
                    .borrow_mut()
                    .insert(source_id, EpgUi::Working);
                false
            }
            EpgState::Done(Ok(_)) => {
                self.epg_status.borrow_mut().remove(&source_id);
                true
            }
            EpgState::Done(Err(error)) => {
                self.epg_status
                    .borrow_mut()
                    .insert(source_id, EpgUi::Failed(error));
                false
            }
        };
        self.show_sources();
        self.update_guide_state();
        if reload && self.source_id.get() == Some(source_id) {
            self.request_now();
            self.request_day();
        }
    }

    /// Stato della guida della sorgente in uso, per la pagina Guida.
    fn update_guide_state(&self) {
        let Some(main) = self.main() else { return };
        let source = self
            .source_id
            .get()
            .and_then(|id| self.sources.borrow().iter().find(|s| s.id == id).cloned());
        let (state, error) = source.map_or((0, String::new()), |s| self.epg_state_of(&s));
        main.set_guide_state(state);
        main.set_guide_error(error.into());
    }

    /// Il canale in onda con «in onda / dopo» aggiornati (Home, player, mini player).
    fn refresh_on_air(&self) {
        if self.main().is_some_and(|m| !m.get_on_demand()) {
            self.set_current(self.current.get());
        }
    }

    /// Minuti da mezzanotte del giorno mostrato (-1 se non è oggi).
    fn update_now_line(&self) {
        let Some(main) = self.main() else { return };
        let minutes = if self.guide_offset.get() == 0 {
            let (_, today) = guide::day_start(0);
            i32::try_from(guide::wall_minutes(unix_now(), today)).unwrap_or(-1)
        } else {
            -1
        };
        main.set_guide_now_min(minutes);
    }

    fn start_guide_timer(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.guide_timer
            .start(slint::TimerMode::Repeated, GUIDE_TICK, move || {
                let Some(app) = weak.upgrade() else { return };
                if unix_now() - app.now_loaded_at.get() >= NOW_WINDOW_RELOAD {
                    app.request_now();
                }
                app.channels.refresh();
                app.refresh_on_air();
                app.update_now_line();
            });
    }

    /// La pagina Guida si apre: giorni aggiornati, vista su adesso e sul canale in onda.
    fn open_guide(&self) {
        let Some(main) = self.main() else { return };
        let days = guide::days();
        let index = days
            .iter()
            .position(|d| i64::from(d.offset) == self.guide_offset.get())
            .unwrap_or(1);
        main.set_guide_days(ModelRc::new(VecModel::from(days)));
        main.set_guide_day(i32::try_from(index).unwrap_or(1));
        self.update_now_line();
        if let Some(row) = self.current.get().and_then(|id| self.channels.row_of(id)) {
            main.set_guide_cursor(count_i32(row));
        }
        main.set_guide_scroll_tick(main.get_guide_scroll_tick().wrapping_add(1));
        // a mezzanotte «oggi» è cambiato
        self.request_day();
    }

    fn pick_guide_day(&self, index: i32) {
        let Some(main) = self.main() else { return };
        let offset = i64::from(index) - 1;
        self.guide_offset.set(offset.clamp(-1, 6));
        main.set_guide_day(index);
        self.update_now_line();
        self.request_day();
        main.set_guide_scroll_tick(main.get_guide_scroll_tick().wrapping_add(1));
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

        // Live
        main.on_play_index(with_app!(|app, row: i32| {
            if let Some((id, _)) = row_index(row).and_then(|r| app.channels.entry_at(r)) {
                app.play_channel(id);
            }
        }));
        main.on_cursor_moved(with_app!(|app, row: i32| app.preview_later(row)));
        main.on_zap(with_app!(|app, delta: i32| app.zap(delta)));
        main.on_pick_category(with_app!(|app, index: i32| {
            let index = row_index(index).unwrap_or(0);
            if app.channels.is_category_hidden(index) {
                app.ask_pin(AfterPin::LiveCategory(index));
                return;
            }
            app.clear_search();
            app.channels.show_category(index);
            app.after_filter_changed();
        }));
        main.on_toggle_fav_channel(with_app!(|app, row: i32| {
            if let Some((_, key)) = row_index(row).and_then(|r| app.channels.entry_at(r)) {
                app.toggle_favorite(ItemKind::Live, key);
            }
        }));
        main.on_toggle_custom_channel(with_app!(|app, row: i32| {
            if let Some((_, key)) = row_index(row).and_then(|r| app.channels.entry_at(r)) {
                app.toggle_custom(key);
            }
        }));
        main.on_search_edited(with_app!(
            |app, text: SharedString| app.search_later(text.to_string())
        ));

        // Home
        main.on_open_resume(with_app!(|app, index: i32| {
            if let Some(index) = row_index(index) {
                app.open_resume(index);
            }
        }));
        main.on_open_favorite(with_app!(|app, index: i32| {
            if let Some(index) = row_index(index) {
                app.open_favorite(index);
            }
        }));

        // Film e serie
        main.on_media_pick_mode(with_app!(|app, mode: i32| app.show_media_mode(mode)));
        main.on_media_pick_category(with_app!(|app, index: i32| {
            let index = row_index(index).unwrap_or(0);
            if app.media_model().is_category_hidden(index) {
                app.ask_pin(AfterPin::MediaCategory(index));
                return;
            }
            app.clear_search();
            app.media_model().show_category(index);
            app.after_media_filter_changed();
        }));
        main.on_media_open(with_app!(|app, row: i32| {
            let kind = if app.media_mode.get() == 1 {
                ItemKind::Series
            } else {
                ItemKind::Movie
            };
            if let Some((id, _)) = row_index(row).and_then(|r| app.media_model().entry_at(r)) {
                app.open_detail(kind, id);
            }
        }));
        main.on_media_back(on(|app| app.close_detail()));
        main.on_media_play(with_app!(|app, from_start: bool| {
            let detail = app.detail.borrow().clone();
            if let Some(d) = detail.filter(|d| d.kind == ItemKind::Movie) {
                let start = if from_start {
                    None
                } else {
                    app.history_entry(ItemKind::Movie, &d.key)
                        .and_then(|e| resume_start(e.position, e.duration))
                };
                app.play_movie(d.id, start);
            }
        }));
        main.on_media_resume_series(on(|app| {
            let detail = app.detail.borrow().clone();
            let Some(d) = detail.filter(|d| d.kind == ItemKind::Series) else {
                return;
            };
            if let Some(entry) = app.history_entry(ItemKind::Series, &d.key) {
                let start = resume_start(entry.position, entry.duration);
                if let Some(episode) = entry.episode {
                    app.play_episode(d.id, episode, start);
                }
            }
        }));
        main.on_media_play_episode(with_app!(|app, index: i32| {
            let detail = app.detail.borrow().clone();
            let Some(d) = detail.filter(|d| d.kind == ItemKind::Series) else {
                return;
            };
            let episodes = app.season_episodes();
            let Some(episode) = row_index(index).and_then(|i| episodes.get(i)) else {
                return;
            };
            // l'episodio lasciato a metà riparte da lì
            let start = app
                .history_entry(ItemKind::Series, &d.key)
                .filter(|e| e.episode.as_ref().is_some_and(|ep| ep.id == episode.id))
                .and_then(|e| resume_start(e.position, e.duration));
            app.play_episode(d.id, episode_ref(episode), start);
        }));
        main.on_media_toggle_fav(on(|app| {
            let detail = app.detail.borrow().clone();
            if let Some(d) = detail {
                app.toggle_favorite(d.kind, d.key);
            }
        }));
        main.on_media_pick_season(with_app!(|app, index: i32| {
            if let Some(d) = app.detail.borrow_mut().as_mut() {
                d.season = row_index(index)
                    .unwrap_or(0)
                    .min(d.seasons.len().saturating_sub(1));
            }
            if let Some(main) = app.main() {
                main.set_media_season(index);
                main.set_media_episode_cursor(0);
            }
            app.refresh_episodes();
        }));

        // player
        main.on_toggle_pause(on(|app| {
            let player = app.router.player();
            let paused = !player.is_paused();
            if let Err(err) = player.set_paused(paused) {
                log::warn!("{err}");
            }
            if let Some(main) = app.main() {
                main.set_paused(paused);
            }
        }));
        main.on_seek(with_app!(|app, secs: i32| {
            if let Err(err) = app.router.player().seek_relative(f64::from(secs)) {
                log::warn!("{err}");
            }
            if let Some(main) = app.main() {
                let position = (main.get_position() + secs).clamp(0, main.get_duration().max(0));
                main.set_position(position);
            }
        }));
        main.on_seek_to(with_app!(|app, fraction: f32| {
            let (_, duration) = app.progress.get();
            if duration > 0.0 {
                let target = duration * f64::from(fraction.clamp(0.0, 1.0));
                if let Err(err) = app.router.player().seek_absolute(target) {
                    log::warn!("{err}");
                }
                if let Some(main) = app.main() {
                    main.set_position(secs_i32(target));
                }
            }
        }));
        main.on_play_next(on(|app| app.play_next()));
        main.on_cancel_next(on(|app| {
            app.stop_countdown();
            app.leave_player();
        }));
        main.on_request_pip(with_app!(|app, row: i32| app.open_pip(row)));
        main.on_close_pip(on(|app| app.router.move_to_main()));
        // Slint cambia il puntatore solo quando il mouse si muove: per farlo sparire col mouse
        // fermo lo si nasconde direttamente nella finestra
        main.on_cursor_hidden(with_app!(|app, hidden: bool| app.set_cursor_hidden(hidden)));
        main.on_toggle_fullscreen(on(|app| {
            if let Some(main) = app.main() {
                app.set_fullscreen(!main.window().is_fullscreen());
                app.auto_fullscreen.set(false);
            }
        }));
        main.on_playing_changed(with_app!(
            |app, playing: bool| app.on_playing_changed(playing)
        ));
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

        // Guida
        main.on_guide_opened(on(|app| app.open_guide()));
        main.on_page_changed(with_app!(|app, page: i32| app.on_page_changed(page)));
        main.on_guide_pick_day(with_app!(|app, index: i32| app.pick_guide_day(index)));
        main.on_guide_watch(with_app!(|app, row: i32| {
            if let Some((id, _)) = row_index(row).and_then(|r| app.channels.entry_at(r)) {
                app.play_channel(id);
            }
        }));
        main.on_guide_refresh(on(|app| {
            app.service.send(Command::RefreshEpg {
                source_id: None,
                force: true,
            });
        }));

        // Impostazioni
        main.on_verify_source(with_app!(
            |app,
             kind: i32,
             name: SharedString,
             location: SharedString,
             username: SharedString,
             password: SharedString,
             ua: SharedString,
             epg: SharedString| {
                app.submit_source(
                    kind,
                    SourceForm {
                        name: &name,
                        location: &location,
                        username: &username,
                        password: &password,
                        user_agent: &ua,
                        epg: &epg,
                    },
                    false,
                )
            }
        ));
        main.on_save_source(with_app!(
            |app,
             kind: i32,
             name: SharedString,
             location: SharedString,
             username: SharedString,
             password: SharedString,
             ua: SharedString,
             epg: SharedString| {
                app.submit_source(
                    kind,
                    SourceForm {
                        name: &name,
                        location: &location,
                        username: &username,
                        password: &password,
                        user_agent: &ua,
                        epg: &epg,
                    },
                    true,
                )
            }
        ));
        main.on_pick_file(on(|app| app.pick_file()));
        main.on_set_source_epg(with_app!(|app, id: i32, url: SharedString| {
            app.service.send(Command::SetSourceEpg {
                source_id: i64::from(id),
                epg_url: Some(url.trim().to_owned()).filter(|u| !u.is_empty()),
            });
        }));
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
            // i testi composti qui (stagioni, durate) si rifanno nella nuova lingua
            app.refresh_detail_info();
            app.refresh_episodes();
        }));
        main.on_set_interval(with_app!(|app, what: i32, index: i32| {
            let hours = row_index(index)
                .and_then(|i| INTERVAL_HOURS.get(i))
                .copied()
                .unwrap_or(0);
            let key = if what == 0 {
                "playlist_hours"
            } else {
                "epg_hours"
            };
            app.save_setting(key, &hours.to_string());
            if let Some(main) = app.main() {
                if what == 0 {
                    main.set_playlist_interval(interval_index(hours));
                } else {
                    main.set_epg_interval(interval_index(hours));
                }
            }
        }));

        // versione nuova
        main.on_open_update(on(|app| app.open_update()));
        main.on_dismiss_update(on(|app| {
            let update = app.update.borrow_mut().take();
            if let Some(update) = update {
                // non la si propone più; la prossima sì
                app.save_setting("update_skipped", &update.version);
            }
            if let Some(main) = app.main() {
                main.set_update_version(SharedString::new());
            }
        }));

        // controllo genitori
        main.on_pin_create(with_app!(
            |app, first: SharedString, again: SharedString| app.create_pin(&first, &again)
        ));
        main.on_pin_remove(on(|app| app.ask_pin(AfterPin::RemovePin)));
        main.on_pin_lock_now(on(|app| {
            app.pin_unlocked.set(false);
            app.set_pin_message(0);
            app.apply_locks();
        }));
        main.on_pin_unlock(on(|app| app.ask_pin(AfterPin::Unlock)));
        main.on_toggle_lock(with_app!(|app, kind: i32, index: i32, locked: bool| {
            app.toggle_lock(kind, index, locked);
        }));
        main.on_pin_submit(with_app!(|app, typed: SharedString| app.submit_pin(&typed)));
        main.on_pin_cancel(on(|app| app.close_pin()));

        main.on_setting_changed(with_app!(|app, key: SharedString, value: SharedString| {
            app.save_setting(&key, &value);
            let settings = app.settings.borrow().clone();
            app.router.set_reconnect(settings.reconnect);
            if key == "custom_name" {
                app.show_custom_name();
            }
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
        pip.on_resize_start(with_app!(|app, corner: i32, x: f32, y: f32| {
            app.start_pip_drag(corner, x, y);
        }));
        pip.on_resize(with_app!(|app, x: f32, y: f32| app.drag_pip(x, y)));
        pip.on_resize_step(with_app!(|app, delta: f32| {
            // +10% per scatto della rotellina (uno scatto vale 60 px); più scatti possono
            // arrivare in un solo evento
            let factor = 1.1f32.powf(delta / WHEEL_NOTCH).clamp(0.5, 2.0);
            app.resize_pip(|w| w * factor);
        }));
        // «chiudi»: chiude il mini player e ferma la riproduzione
        pip.on_dismiss(on(|app| {
            app.stop_playback();
            app.router.move_to_main();
        }));
        // «espandi»: riprende il contenuto a tutto schermo nella finestra principale
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

    /// Apre nel browser la pagina della versione nuova (solo pagine delle Release di Sintonia).
    fn open_update(&self) {
        let Some(url) = self.update.borrow().as_ref().map(|u| u.url.clone()) else {
            return;
        };
        if !update::is_release_page(&url) {
            return;
        }
        #[cfg(windows)]
        let opened = std::process::Command::new("explorer").arg(&url).spawn();
        #[cfg(target_os = "macos")]
        let opened = std::process::Command::new("open").arg(&url).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let opened = std::process::Command::new("xdg-open").arg(&url).spawn();
        if let Err(err) = opened {
            log::warn!("pagina della versione nuova: {err}");
        }
    }

    // ───────────── controllo genitori ─────────────

    /// Le categorie bloccate vanno nascoste: c'è un PIN e non è stato inserito.
    fn hide_locked(&self) -> bool {
        self.settings.borrow().has_pin() && !self.pin_unlocked.get()
    }

    /// Le categorie bloccate della sorgente in uso.
    fn source_locks(&self) -> Locks {
        self.source_id
            .get()
            .and_then(|id| self.settings.borrow().locks.get(&id).cloned())
            .unwrap_or_default()
    }

    /// Passa i blocchi ai modelli (senza aggiornare il resto dell'interfaccia).
    fn set_model_locks(&self) {
        let locks = self.source_locks();
        let custom = self
            .source_id
            .get()
            .and_then(|id| self.settings.borrow().custom.get(&id).cloned())
            .unwrap_or_default();
        let pin = self.settings.borrow().has_pin();
        let hide = self.hide_locked();
        self.channels.set_locks(locks.live, custom, pin, hide);
        self.movies
            .set_locks(locks.movies, HashSet::new(), pin, hide);
        self.series
            .set_locks(locks.series, HashSet::new(), pin, hide);
    }

    /// Applica PIN e blocchi a tutta l'interfaccia. Se si stava guardando qualcosa di una
    /// categoria appena nascosta, si ferma.
    fn apply_locks(&self) {
        self.set_model_locks();
        let Some(main) = self.main() else { return };
        main.set_pin_set(self.settings.borrow().has_pin());
        main.set_pin_unlocked(self.pin_unlocked.get());
        let playing_hidden = self
            .now
            .borrow()
            .as_ref()
            .is_some_and(|now| match now.kind {
                ItemKind::Live => self.channels.is_id_hidden(self.current.get().unwrap_or(-1)),
                ItemKind::Movie => self.movies.id_of_key(&now.key).is_none(),
                ItemKind::Series => self.series.id_of_key(&now.key).is_none(),
            });
        if playing_hidden {
            self.stop_playback();
            main.set_playing(false);
            if let Some(pip) = self.pip.upgrade() {
                if let Err(err) = pip.hide() {
                    log::warn!("{err}");
                }
            }
            self.router.move_to_main();
        }
        let detail_hidden = self.detail.borrow().as_ref().is_some_and(|d| {
            let model = if d.kind == ItemKind::Movie {
                &self.movies
            } else {
                &self.series
            };
            model.is_id_hidden(d.id)
        });
        if detail_hidden {
            self.close_detail();
        }
        let current = self
            .current
            .get()
            .filter(|id| !self.channels.is_id_hidden(*id))
            .or_else(|| self.channels.first_id());
        self.after_channels_changed(current);
        self.show_media_mode(self.media_mode.get());
        self.refresh_home();
        self.refresh_lock_lists();
    }

    /// Le categorie di canali, film e serie nelle Impostazioni (senza «Tutti» e «Preferiti»).
    fn refresh_lock_lists(&self) {
        let Some(main) = self.main() else { return };
        // la categoria personalizzata non c'è: con il PIN è sempre bloccata
        let list = |categories: Vec<CategoryItem>, first: usize| {
            ModelRc::new(VecModel::from(
                categories.into_iter().skip(first).collect::<Vec<_>>(),
            ))
        };
        main.set_lock_live(list(
            self.channels.categories(),
            self.channels.first_group(),
        ));
        main.set_lock_movies(list(self.movies.categories(), self.movies.first_group()));
        main.set_lock_series(list(self.series.categories(), self.series.first_group()));
    }

    /// Mette o toglie un canale dalla categoria personalizzata della sorgente in uso. Con il
    /// PIN non inserito il canale aggiunto sparisce subito, come quelli delle categorie bloccate.
    fn toggle_custom(&self, key: String) {
        let Some(source_id) = self.source_id.get() else {
            return;
        };
        let value = {
            let mut settings = self.settings.borrow_mut();
            let keys = settings.custom.entry(source_id).or_default();
            if !keys.remove(&key) {
                keys.insert(key);
            }
            settings::custom_value(keys)
        };
        self.save_setting(&settings::custom_key(source_id), &value);
        let row = self.main().map(|m| m.get_cursor());
        self.apply_locks();
        // l'elenco resta dov'era (se il canale è sparito, sulla riga dopo)
        if let (Some(main), Some(row)) = (self.main(), row) {
            let last = count_i32(self.channels.row_count().saturating_sub(1));
            main.set_cursor(row.min(last).max(0));
        }
    }

    /// Il nome della categoria personalizzata, ovunque appaia.
    fn show_custom_name(&self) {
        if let Some(main) = self.main() {
            let name = self.settings.borrow().custom_name.clone();
            main.global::<Texts>().set_custom_name(name.as_str().into());
            main.set_custom_name(name.into());
        }
    }

    fn set_pin_message(&self, message: i32) {
        if let Some(main) = self.main() {
            main.set_pin_message(message);
        }
    }

    fn create_pin(&self, first: &str, again: &str) {
        let (first, again) = (first.trim(), again.trim());
        if !pin::is_valid(first) {
            self.set_pin_message(2);
            return;
        }
        if first != again {
            self.set_pin_message(1);
            return;
        }
        match pin::hash(first) {
            Ok(stored) => {
                self.save_setting("pin", &stored);
                // chi ha appena scelto il PIN lo conosce: si parte sbloccati
                self.pin_unlocked.set(true);
                self.set_pin_message(3);
                self.apply_locks();
            }
            Err(err) => log::error!("PIN: {err}"),
        }
    }

    fn toggle_lock(&self, kind: i32, index: i32, locked: bool) {
        let Some(source_id) = self.source_id.get() else {
            return;
        };
        if !self.pin_unlocked.get() {
            self.apply_locks();
            return;
        }
        let (kind, categories, first) = match kind {
            1 => (
                ItemKind::Movie,
                self.movies.categories(),
                self.movies.first_group(),
            ),
            2 => (
                ItemKind::Series,
                self.series.categories(),
                self.series.first_group(),
            ),
            _ => (
                ItemKind::Live,
                self.channels.categories(),
                self.channels.first_group(),
            ),
        };
        let Some(category) = row_index(index).and_then(|i| categories.get(i + first)) else {
            return;
        };
        let value = {
            let mut settings = self.settings.borrow_mut();
            let locks = settings.locks.entry(source_id).or_default();
            locks.set(kind, category.name.as_str(), locked);
            locks.to_value()
        };
        self.save_setting(&Locks::key(source_id), &value);
        self.apply_locks();
    }

    fn ask_pin(&self, after: AfterPin) {
        let Some(main) = self.main() else { return };
        self.after_pin.set(Some(after));
        main.set_pin_reason(i32::from(after == AfterPin::RemovePin));
        main.set_pin_error(false);
        main.set_pin_prompt(true);
    }

    fn close_pin(&self) {
        self.after_pin.set(None);
        if let Some(main) = self.main() {
            main.set_pin_prompt(false);
            main.set_pin_error(false);
        }
    }

    fn submit_pin(&self, typed: &str) {
        let Some(main) = self.main() else { return };
        let Some(after) = self.after_pin.get() else {
            self.close_pin();
            return;
        };
        let ok = self
            .settings
            .borrow()
            .pin
            .as_deref()
            .is_some_and(|stored| pin::verify(typed.trim(), stored));
        if !ok {
            // `changed error` svuota la casella: si passa da falso a vero anche dopo un errore
            main.set_pin_error(false);
            main.set_pin_error(true);
            return;
        }
        self.close_pin();
        if after == AfterPin::RemovePin {
            self.save_setting("pin", "");
            self.set_pin_message(4);
            self.apply_locks();
            return;
        }
        self.pin_unlocked.set(true);
        self.set_pin_message(0);
        self.apply_locks();
        match after {
            AfterPin::Unlock | AfterPin::RemovePin => {}
            AfterPin::LiveCategory(index) => {
                self.clear_search();
                self.channels.show_category(index);
                self.after_filter_changed();
            }
            AfterPin::MediaCategory(index) => {
                self.clear_search();
                self.media_model().show_category(index);
                self.after_media_filter_changed();
            }
        }
    }

    fn preview_later(self: &Rc<Self>, row: i32) {
        if !self.settings.borrow().live_preview {
            return;
        }
        // con il mini player aperto l'elenco non cambia quello che si sta guardando
        if self.pip.upgrade().is_some_and(|p| p.window().is_visible()) {
            return;
        }
        let Some((id, _)) = row_index(row).and_then(|r| self.channels.entry_at(r)) else {
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
        if let Some((id, _)) = self.channels.entry_at(next) {
            self.play_channel(id);
            if let Some(main) = self.main() {
                main.set_cursor(count_i32(next));
            }
        }
    }

    /// Svuota la ricerca e riporta gli elenchi alle loro categorie.
    fn clear_search(&self) {
        self.search_timer.stop();
        self.search_generation.set(self.search_generation.get() + 1);
        if let Some(main) = self.main() {
            if !main.get_search().is_empty() {
                main.set_search(SharedString::new());
            }
        }
        self.channels.show_category(self.channels.category());
        self.movies.show_category(self.movies.category());
        self.series.show_category(self.series.category());
    }

    fn search_later(self: &Rc<Self>, text: String) {
        let generation = self.search_generation.get() + 1;
        self.search_generation.set(generation);
        if text.trim().is_empty() {
            self.search_timer.stop();
            self.channels.show_category(self.channels.category());
            self.movies.show_category(self.movies.category());
            self.series.show_category(self.series.category());
            self.after_filter_changed();
            self.after_media_filter_changed();
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
        let picked = row_index(row)
            .and_then(|r| self.channels.entry_at(r))
            .map(|(id, _)| id);
        match picked {
            Some(id) => self.play_channel(id),
            // un film o un episodio continua nel mini player
            None if main.get_on_demand() => {}
            None => match self.current.get() {
                Some(id) => self.play_channel(id),
                None => return,
            },
        }
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

    /// Il mouse in coordinate dello schermo, dal punto `(x, y)` della finestra del mini player.
    fn pip_cursor(window: &slint::Window, x: f32, y: f32) -> (f32, f32) {
        let (scale, pos) = (window.scale_factor(), window.position());
        (pos.x as f32 + x * scale, pos.y as f32 + y * scale)
    }

    /// Pressione su un angolo (0 alto-sinistra, 1 alto-destra, 2 basso-sinistra,
    /// 3 basso-destra); `(x, y)` è il mouse nella finestra.
    fn start_pip_drag(&self, corner: i32, x: f32, y: f32) {
        let Some(pip) = self.pip.upgrade() else {
            return;
        };
        let window = pip.window();
        self.pip_drag.set(Some(PipDrag {
            left: corner == 0 || corner == 2,
            top: corner == 0 || corner == 1,
            pos: window.position(),
            width: window.size().to_logical(window.scale_factor()).width,
            cursor: Self::pip_cursor(window, x, y),
        }));
    }

    /// Trascinamento di un angolo: la misura nasce dallo spostamento del mouse rispetto alla
    /// pressione; l'angolo opposto resta fermo.
    fn drag_pip(self: &Rc<Self>, x: f32, y: f32) {
        let (Some(pip), Some(drag)) = (self.pip.upgrade(), self.pip_drag.get()) else {
            return;
        };
        let window = pip.window();
        let scale = window.scale_factor();
        let cursor = Self::pip_cursor(window, x, y);
        let (dx, dy) = (
            (cursor.0 - drag.cursor.0) / scale,
            (cursor.1 - drag.cursor.1) / scale,
        );
        // la larghezza segue il movimento più ampio, l'altezza resta in proporzione
        let from_x = if drag.left { -dx } else { dx };
        let from_y = (if drag.top { -dy } else { dy }) / PIP_RATIO;
        let delta = if from_x.abs() >= from_y.abs() {
            from_x
        } else {
            from_y
        };
        let width = (drag.width + delta)
            .round()
            .clamp(PIP_MIN_WIDTH, PIP_MAX_WIDTH);
        let current = window.size().to_logical(scale).width;
        if (width - current).abs() < 0.5 {
            return;
        }
        let height = width * PIP_RATIO;
        let start_height = drag.width * PIP_RATIO;
        window.set_size(slint::LogicalSize::new(width, height));
        if drag.left || drag.top {
            let shift_x = if drag.left { drag.width - width } else { 0.0 };
            let shift_y = if drag.top { start_height - height } else { 0.0 };
            window.set_position(PhysicalPosition::new(
                drag.pos.x + (shift_x * scale).round() as i32,
                drag.pos.y + (shift_y * scale).round() as i32,
            ));
        }
        self.save_pip_width_later(width);
    }

    /// Rotellina: cambia la larghezza del mini player (l'altezza segue in 16:9) tenendo fermo
    /// il centro.
    fn resize_pip(self: &Rc<Self>, new_width: impl FnOnce(f32) -> f32) {
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
        window.set_position(PhysicalPosition::new(
            pos.x - (dw / 2.0 * scale).round() as i32,
            pos.y - (dh / 2.0 * scale).round() as i32,
        ));
        self.save_pip_width_later(width);
    }

    /// Salva la larghezza del mini player poco dopo l'ultimo cambiamento.
    fn save_pip_width_later(self: &Rc<Self>, width: f32) {
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

    /// Nasconde (o rimostra) il puntatore del mouse sopra la finestra principale.
    fn set_cursor_hidden(&self, hidden: bool) {
        if let Some(main) = self.main() {
            main.window()
                .with_winit_window(|window| window.set_cursor_visible(!hidden));
        }
    }

    /// Schermo intero sì o no; l'interfaccia lo sa per mostrare il pulsante giusto.
    fn set_fullscreen(&self, on: bool) {
        if let Some(main) = self.main() {
            main.window().set_fullscreen(on);
            main.set_fullscreen(on);
        }
    }

    fn on_playing_changed(&self, playing: bool) {
        let Some(main) = self.main() else { return };
        if !playing {
            // il player si chiude: il puntatore torna in ogni caso
            self.set_cursor_hidden(false);
        }
        let window = main.window();
        if playing && self.settings.borrow().fullscreen_start && !window.is_fullscreen() {
            self.set_fullscreen(true);
            self.auto_fullscreen.set(true);
        } else if !playing && self.auto_fullscreen.replace(false) {
            self.set_fullscreen(false);
        }
        let pip_open = self.pip.upgrade().is_some_and(|p| p.window().is_visible());
        if playing {
            // il player riprende il canale in onda, se era fermo (film ed episodi partono da sé)
            if !main.get_on_demand() {
                if let Some(id) = self.current.get() {
                    self.play_channel(id);
                    // l'elenco canali del player si apre sul canale in onda
                    if let Some(row) = self.channels.row_of(id) {
                        main.set_cursor(count_i32(row));
                    }
                }
            }
        } else if main.get_on_demand() && !pip_open {
            // un film non continua dietro le pagine: si ferma e si riprende da lì
            self.stop_playback();
        }
    }

    fn submit_source(&self, kind: i32, form: SourceForm<'_>, save: bool) {
        let Some(main) = self.main() else { return };
        let location = form.location.trim();
        let username = form.username.trim();
        let xtream = kind == 2;
        if location.is_empty() || (xtream && (username.is_empty() || form.password.is_empty())) {
            main.set_source_status(SourceStatus {
                state: 4,
                error: 7,
                ..Default::default()
            });
            return;
        }
        let name = match form.name.trim() {
            "" if xtream => "Xtream Codes".to_owned(),
            "" => "Playlist".to_owned(),
            name => name.to_owned(),
        };
        let user_agent = Some(form.user_agent.trim())
            .filter(|ua| !ua.is_empty())
            .map(str::to_owned);
        let source = NewSource {
            name,
            kind: match kind {
                1 => SourceKind::M3uFile,
                2 => SourceKind::Xtream,
                _ => SourceKind::M3uUrl,
            },
            location: location.to_owned(),
            username: xtream.then(|| username.to_owned()),
            user_agent,
            epg_url: Some(form.epg.trim())
                .filter(|e| !e.is_empty())
                .map(str::to_owned),
        };
        let command = match (xtream, save) {
            (true, true) => Command::AddXtream {
                source,
                password: form.password.to_owned(),
            },
            (true, false) => Command::VerifyXtream {
                source,
                password: form.password.to_owned(),
            },
            (false, true) => Command::AddSource(source),
            (false, false) => Command::VerifySource(source),
        };
        self.service.send(command);
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

/// I campi del modulo «Aggiungi sorgente».
#[derive(Debug, Clone, Copy)]
struct SourceForm<'a> {
    name: &'a str,
    location: &'a str,
    username: &'a str,
    password: &'a str,
    user_agent: &'a str,
    epg: &'a str,
}

fn count_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

fn row_index(row: i32) -> Option<usize> {
    usize::try_from(row).ok()
}

/// Posizione di un intervallo nell'elenco delle Impostazioni (il più vicino, se non c'è).
fn interval_index(hours: u32) -> i32 {
    let nearest = INTERVAL_HOURS
        .iter()
        .enumerate()
        .min_by_key(|(_, h)| h.abs_diff(hours))
        .map_or(0, |(i, _)| i);
    count_i32(nearest)
}

fn secs_i32(secs: f64) -> i32 {
    secs.clamp(0.0, f64::from(i32::MAX)) as i32
}

/// «S1 · E3».
fn episode_label(season: u32, number: u32) -> String {
    format!("S{season} · E{number}")
}

fn rating_label(rating: &str) -> String {
    if rating.is_empty() {
        String::new()
    } else {
        format!("★ {rating}")
    }
}

fn episode_ref(episode: &Episode) -> EpisodeRef {
    EpisodeRef {
        id: episode.id,
        season: episode.season,
        number: episode.number,
        container: episode.container.clone(),
    }
}

/// Visto (quasi) fino in fondo.
fn is_finished(position: f64, duration: f64) -> bool {
    duration > 0.0
        && (position / duration >= END_FRACTION
            || (duration > 10.0 * END_MARGIN_SECS && duration - position < END_MARGIN_SECS))
}

/// Da dove riprendere: `None` (dall'inizio) se visto poco o finito.
fn resume_start(position: f64, duration: f64) -> Option<u32> {
    (position >= RESUME_MIN_SECS && !is_finished(position, duration))
        .then(|| position.min(f64::from(u32::MAX)) as u32)
}

/// Come appare un film o un episodio al posto del canale in onda.
fn on_demand_item(now: &NowPlaying) -> ChannelItem {
    ChannelItem {
        id: -1,
        name: now.title.as_str().into(),
        group: now.subtitle.as_str().into(),
        tint: tint_for(&now.title),
        ..ChannelItem::default()
    }
}

fn source_error_status(err: &SourceError) -> SourceStatus {
    let (error, http, detail) = match err {
        SourceError::Http(code) => (1, i32::from(*code), String::new()),
        SourceError::Network(d) => (2, 0, d.clone()),
        SourceError::File(d) => (3, 0, d.clone()),
        SourceError::Empty => (4, 0, String::new()),
        SourceError::TooLarge => (5, 0, String::new()),
        SourceError::Store(d) => (6, 0, d.clone()),
        SourceError::Auth => (8, 0, String::new()),
        SourceError::InvalidServer => (9, 0, String::new()),
        SourceError::Format(d) => (10, 0, d.clone()),
        SourceError::Secrets(d) => (11, 0, d.clone()),
    };
    SourceStatus {
        state: 4,
        error,
        http,
        detail: detail.into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_and_resume_points() {
        // film di 100 minuti
        let d = 6000.0;
        assert!(!is_finished(3000.0, d));
        assert!(is_finished(5950.0, d), "titoli di coda");
        assert!(is_finished(5800.0, d), "oltre il 95%");
        assert!(!is_finished(100.0, 0.0), "durata sconosciuta");
        assert_eq!(resume_start(10.0, d), None, "visto troppo poco");
        assert_eq!(resume_start(1234.7, d), Some(1234));
        assert_eq!(resume_start(5990.0, d), None, "finito: dall'inizio");
        // episodio breve: niente margine fisso
        assert!(!is_finished(250.0, 300.0));
    }

    #[test]
    fn interval_positions() {
        assert_eq!(interval_index(0), 0);
        assert_eq!(interval_index(24), 3);
        assert_eq!(interval_index(168), 5);
        assert_eq!(interval_index(20), 3, "il più vicino");
        assert_eq!(interval_index(1000), 5);
    }

    #[test]
    fn labels() {
        assert_eq!(episode_label(2, 4), "S2 · E4");
        assert_eq!(rating_label(""), "");
        assert_eq!(rating_label("7.5"), "★ 7.5");
        assert_eq!(secs_i32(-3.0), 0);
        assert_eq!(secs_i32(61.9), 61);
    }
}
