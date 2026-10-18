//! Involucro di libmpv per Sintonia.
//!
//! - [`Player`] comanda la riproduzione (carica, ferma, pausa) e consegna gli eventi di mpv a una
//!   funzione dell'app, da un thread dedicato.
//! - [`RenderContext`] collega mpv a un contesto OpenGL già esistente (quello di una finestra
//!   Slint) e disegna il fotogramma corrente in un framebuffer.
//!
//! La DLL deve essere una build **LGPL** di libmpv: vedi `build.rs`.
//! I messaggi di mpv passano per [`sintonia_core::redact`] prima di finire nel log, perché
//! gli URL IPTV contengono spesso utente e password.

mod ffi;
mod render;

use std::ffi::{c_char, c_int, CStr, CString, NulError};
use std::fmt;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

pub use render::RenderContext;

/// Errori del player.
#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    /// `mpv_create()` ha restituito NULL (memoria esaurita o libreria non utilizzabile).
    #[error("impossibile creare l'istanza di mpv")]
    Create,
    /// Una chiamata di libmpv è fallita.
    #[error("mpv, {what}: {message} ({code})")]
    Mpv {
        /// Cosa si stava facendo.
        what: &'static str,
        /// Codice d'errore di mpv (`MPV_ERROR_*`).
        code: i32,
        /// Descrizione di mpv.
        message: String,
    },
    /// La libmpv trovata è di una versione che non supportiamo.
    #[error("libmpv con API {major}.{minor}: serve la 2.x")]
    ApiVersion {
        /// Versione principale trovata.
        major: u64,
        /// Versione secondaria trovata.
        minor: u64,
    },
    /// Una stringa passata a mpv contiene un carattere nullo.
    #[error("testo non valido per mpv (contiene un carattere nullo)")]
    Nul(#[from] NulError),
    /// Il thread degli eventi non è partito.
    #[error("impossibile avviare il thread degli eventi di mpv: {0}")]
    Thread(#[source] std::io::Error),
}

/// Impostazioni fissate alla creazione del player.
#[derive(Debug, Clone)]
pub struct PlayerConfig {
    /// User-agent HTTP; `None` lascia quello di mpv.
    pub user_agent: Option<String>,
    /// Decodifica hardware (`hwdec=auto-safe`).
    pub hardware_decoding: bool,
    /// Livello minimo dei messaggi di mpv da mandare al log (`"warn"`, `"info"`, `"v"`…).
    pub log_level: &'static str,
    /// Secondi dopo cui mpv considera persa una connessione.
    pub network_timeout_secs: u32,
    /// Memoria massima per i dati in arrivo (MiB). Per la TV in diretta bastano pochi secondi.
    pub cache_mib: u32,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            user_agent: None,
            hardware_decoding: true,
            log_level: "warn",
            network_timeout_secs: 15,
            cache_mib: 48,
        }
    }
}

/// Opzioni di un singolo flusso (dalla playlist).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct LoadOptions {
    /// User-agent richiesto dal canale (ha la precedenza su quello generale).
    pub user_agent: Option<String>,
    /// Referrer richiesto dal canale.
    pub referrer: Option<String>,
    /// Secondo da cui partire (per riprendere un film o un episodio).
    pub start_secs: Option<u32>,
}

impl fmt::Debug for LoadOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadOptions")
            .field("user_agent", &self.user_agent.is_some())
            .field("referrer", &self.referrer.is_some())
            .finish()
    }
}

/// Tipo di traccia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    /// Audio.
    Audio,
    /// Sottotitoli.
    Subtitle,
}

/// Una traccia audio o di sottotitoli del flusso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// Identificativo di mpv (per [`Player::select_track`]).
    pub id: i64,
    /// Tipo.
    pub kind: TrackKind,
    /// Lingua (codice, es. `ita`), se dichiarata.
    pub lang: Option<String>,
    /// Titolo, se dichiarato.
    pub title: Option<String>,
    /// Codec (es. `ac3`).
    pub codec: Option<String>,
    /// Selezionata adesso.
    pub selected: bool,
}

/// Attese crescenti tra un tentativo di riconnessione e l'altro: 1, 2, 4, 8, 16, poi 30 secondi.
#[derive(Debug, Clone, Default)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    /// Tentativi dopo cui si rinuncia (circa tre minuti in tutto).
    pub const MAX_ATTEMPTS: u32 = 10;

    /// Attesa prima del prossimo tentativo, o `None` se si deve rinunciare.
    pub fn next_delay(&mut self) -> Option<std::time::Duration> {
        if self.attempt >= Self::MAX_ATTEMPTS {
            return None;
        }
        let secs = 1u64.checked_shl(self.attempt).map_or(30, |s| s.min(30));
        self.attempt += 1;
        Some(std::time::Duration::from_secs(secs))
    }

    /// Tentativi fatti finora.
    pub fn attempts(&self) -> u32 {
        self.attempt
    }

    /// Da chiamare quando il flusso riparte.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// Valore per le opzioni per-file di `loadfile`: `%n%testo` evita qualsiasi problema con
/// virgole, uguali e virgolette.
fn quote_option(value: &str) -> String {
    format!("%{}%{value}", value.len())
}

fn load_options_string(options: &LoadOptions) -> String {
    let mut parts = Vec::new();
    if let Some(ua) = &options.user_agent {
        parts.push(format!("user-agent={}", quote_option(ua)));
    }
    if let Some(referrer) = &options.referrer {
        parts.push(format!("referrer={}", quote_option(referrer)));
    }
    if let Some(start) = options.start_secs.filter(|s| *s > 0) {
        parts.push(format!("start={start}"));
    }
    parts.join(",")
}

/// Perché è finita la riproduzione di un flusso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// Il file è finito (per i flussi live di solito vuol dire che è caduta la connessione).
    Eof,
    /// Fermato da noi (`stop` o un nuovo `load`).
    Stopped,
    /// Errore; il testo è quello di mpv e non contiene URL.
    Error(String),
}

/// Eventi che il player manda all'app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// mpv ha iniziato ad aprire un flusso.
    Loading,
    /// Il flusso è aperto (formato riconosciuto).
    Loaded,
    /// La riproduzione è partita o ripartita (dopo un caricamento o un salto).
    Playing,
    /// Dimensioni del video, già corrette per l'aspetto.
    VideoSize {
        /// Larghezza in pixel.
        width: u32,
        /// Altezza in pixel.
        height: u32,
    },
    /// In attesa di dati dalla rete (`true`) o ripartito (`false`).
    Buffering(bool),
    /// Fine del flusso.
    Ended(EndReason),
    /// Nessun flusso caricato.
    Idle,
}

/// Il core di mpv. Si distrugge solo quando nessuno lo usa più: né il [`Player`], né il thread
/// degli eventi, né un [`RenderContext`] (che deve essere liberato prima del core).
pub(crate) struct Core {
    handle: NonNull<ffi::mpv_handle>,
}

// SAFETY: l'API client di libmpv è thread-safe: tutte le funzioni `mpv_*` sul core (esclusi i
// `mpv_render_*`, che stanno in `RenderContext`) si possono chiamare da qualsiasi thread.
unsafe impl Send for Core {}
// SAFETY: vedi sopra; il core non ha stato Rust mutabile.
unsafe impl Sync for Core {}

impl Core {
    pub(crate) fn as_ptr(&self) -> *mut ffi::mpv_handle {
        self.handle.as_ptr()
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // SAFETY: `handle` viene da `mpv_create()` ed è l'ultimo riferimento (Arc): nessun altro
        // thread lo usa e un eventuale RenderContext, che tiene anch'esso un Arc, è già stato liberato.
        unsafe { ffi::mpv_terminate_destroy(self.handle.as_ptr()) };
    }
}

/// Il player: comandi verso mpv ed eventi verso l'app.
pub struct Player {
    core: Arc<Core>,
    stop: Arc<AtomicBool>,
    events: Option<JoinHandle<()>>,
}

impl fmt::Debug for Player {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

/// Versione dell'API client di libmpv caricata, come `(principale, secondaria)`.
pub fn api_version() -> (u64, u64) {
    // SAFETY: funzione senza argomenti e senza stato.
    let v = unsafe { ffi::mpv_client_api_version() };
    (v >> 16, v & 0xffff)
}

fn mpv_error(what: &'static str, code: c_int) -> PlayerError {
    // SAFETY: mpv_error_string restituisce sempre una stringa statica terminata da zero,
    // anche per codici sconosciuti.
    let message = unsafe { CStr::from_ptr(ffi::mpv_error_string(code)) }
        .to_string_lossy()
        .into_owned();
    PlayerError::Mpv {
        what,
        code,
        message,
    }
}

fn check(what: &'static str, code: c_int) -> Result<(), PlayerError> {
    if code < 0 {
        Err(mpv_error(what, code))
    } else {
        Ok(())
    }
}

/// User-agent quando né le impostazioni né il canale ne indicano uno.
pub const DEFAULT_USER_AGENT: &str = concat!("Sintonia/", env!("CARGO_PKG_VERSION"), " (libmpv)");

/// Identificativi delle proprietà osservate (`reply_userdata`).
const OBSERVE_PAUSED_FOR_CACHE: u64 = 1;

impl Player {
    /// Crea e avvia mpv. `on_event` viene chiamata dal thread degli eventi di mpv, non da quello
    /// dell'interfaccia: deve solo inoltrare (per esempio con `slint::invoke_from_event_loop`).
    pub fn new<F>(config: &PlayerConfig, on_event: F) -> Result<Self, PlayerError>
    where
        F: FnMut(PlayerEvent) + Send + 'static,
    {
        let (major, minor) = api_version();
        if major != 2 {
            return Err(PlayerError::ApiVersion { major, minor });
        }

        // SAFETY: nessun prerequisito; NULL viene gestito subito sotto.
        let handle = NonNull::new(unsafe { ffi::mpv_create() }).ok_or(PlayerError::Create)?;
        let core = Arc::new(Core { handle });

        let mut options: Vec<(&str, String)> = vec![
            // disegniamo noi tramite la render API: mai una finestra di mpv
            ("vo", "libmpv".into()),
            ("idle", "yes".into()),
            ("terminal", "no".into()),
            ("config", "no".into()),
            ("load-scripts", "no".into()),
            ("ytdl", "no".into()),
            ("osc", "no".into()),
            ("osd-level", "0".into()),
            ("input-default-bindings", "no".into()),
            ("input-vo-keyboard", "no".into()),
            ("keep-open", "no".into()),
            ("audio-client-name", "Sintonia".into()),
            (
                "hwdec",
                if config.hardware_decoding {
                    "auto-safe"
                } else {
                    "no"
                }
                .into(),
            ),
            ("network-timeout", config.network_timeout_secs.to_string()),
            ("cache", "yes".into()),
            ("demuxer-max-bytes", format!("{}MiB", config.cache_mib)),
            ("demuxer-max-back-bytes", format!("{}MiB", (config.cache_mib / 4).max(1))),
            // HLS: con le richieste HTTP in parallelo di FFmpeg alcuni server (es. gruppo Discovery)
            // impiegano 25-30 s a partire; senza, circa il 40% in meno, e gli altri canali non
            // cambiano (misurato con l'esempio `probe`)
            ("demuxer-lavf-o", "http_multiple=0".into()),
            // FFmpeg riprova da sé le connessioni HTTP che cadono, prima che intervenga l'app
            (
                "stream-lavf-o",
                "reconnect=1,reconnect_streamed=1,reconnect_on_network_error=1,reconnect_delay_max=4".into(),
            ),
        ];
        options.push((
            "user-agent",
            config
                .user_agent
                .clone()
                .unwrap_or_else(|| DEFAULT_USER_AGENT.to_owned()),
        ));
        for (name, value) in options {
            let (name_c, value_c) = (CString::new(name)?, CString::new(value)?);
            // SAFETY: core valido e non ancora inizializzato; le stringhe vivono fino al ritorno.
            let code = unsafe {
                ffi::mpv_set_option_string(core.as_ptr(), name_c.as_ptr(), value_c.as_ptr())
            };
            if code < 0 {
                // un'opzione mancante in una build ridotta non deve impedire l'avvio
                log::warn!(
                    "opzione di mpv '{name}' rifiutata: {}",
                    mpv_error("opzione", code)
                );
            }
        }

        // SAFETY: core valido, opzioni impostate.
        check("avvio", unsafe { ffi::mpv_initialize(core.as_ptr()) })?;

        let level = CString::new(config.log_level)?;
        // SAFETY: core valido; la stringa vive fino al ritorno.
        check("richiesta dei messaggi", unsafe {
            ffi::mpv_request_log_messages(core.as_ptr(), level.as_ptr())
        })?;
        // SAFETY: core valido; il nome è una stringa statica.
        check("osservazione di paused-for-cache", unsafe {
            ffi::mpv_observe_property(
                core.as_ptr(),
                OBSERVE_PAUSED_FOR_CACHE,
                c"paused-for-cache".as_ptr(),
                ffi::MPV_FORMAT_FLAG,
            )
        })?;

        let stop = Arc::new(AtomicBool::new(false));
        let events = std::thread::Builder::new()
            .name("mpv-eventi".into())
            .spawn({
                let core = Arc::clone(&core);
                let stop = Arc::clone(&stop);
                move || event_loop(&core, &stop, on_event)
            })
            .map_err(PlayerError::Thread)?;

        Ok(Self {
            core,
            stop,
            events: Some(events),
        })
    }

    pub(crate) fn core(&self) -> &Arc<Core> {
        &self.core
    }

    fn command(&self, what: &'static str, args: &[&str]) -> Result<(), PlayerError> {
        let owned = args
            .iter()
            .map(|a| CString::new(*a))
            .collect::<Result<Vec<_>, _>>()?;
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|a| a.as_ptr()).collect();
        ptrs.push(ptr::null());
        // SAFETY: core valido; `ptrs` è un array di stringhe C terminato da NULL che vive
        // (insieme a `owned`) fino al ritorno; mpv copia gli argomenti.
        check(what, unsafe {
            ffi::mpv_command(self.core.as_ptr(), ptrs.as_mut_ptr())
        })
    }

    fn set_property(&self, what: &'static str, name: &str, value: &str) -> Result<(), PlayerError> {
        let (name, value) = (CString::new(name)?, CString::new(value)?);
        // SAFETY: core valido; le stringhe vivono fino al ritorno e mpv le copia.
        check(what, unsafe {
            ffi::mpv_set_property_string(self.core.as_ptr(), name.as_ptr(), value.as_ptr())
        })
    }

    /// Apre un flusso (URL o file) al posto di quello corrente. Ritorna subito: l'esito arriva
    /// con gli eventi.
    pub fn load(&self, url: &str) -> Result<(), PlayerError> {
        self.load_with(url, &LoadOptions::default())
    }

    /// Come [`Self::load`], con user-agent e referrer del singolo canale.
    pub fn load_with(&self, url: &str, options: &LoadOptions) -> Result<(), PlayerError> {
        log::info!("carico {}", sintonia_core::redact::redact_url(url));
        let per_file = load_options_string(options);
        if per_file.is_empty() {
            self.command("caricamento", &["loadfile", url, "replace"])
        } else {
            // loadfile <url> <flag> <indice> <opzioni>: l'indice -1 è ignorato con "replace"
            self.command(
                "caricamento",
                &["loadfile", url, "replace", "-1", &per_file],
            )
        }
    }

    /// User-agent generale (per i canali che non ne indicano uno).
    pub fn set_user_agent(&self, user_agent: Option<&str>) -> Result<(), PlayerError> {
        self.set_property(
            "user-agent",
            "user-agent",
            user_agent.unwrap_or(DEFAULT_USER_AGENT),
        )
    }

    /// Legge una proprietà di mpv come testo.
    pub fn property(&self, name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        get_string(&self.core, &name)
    }

    /// Tracce audio e sottotitoli del flusso corrente.
    pub fn tracks(&self) -> Vec<Track> {
        let count: usize = self
            .property("track-list/count")
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        (0..count)
            .filter_map(|i| {
                let field = |name: &str| {
                    self.property(&format!("track-list/{i}/{name}"))
                        .filter(|v| !v.is_empty())
                };
                let kind = match field("type")?.as_str() {
                    "audio" => TrackKind::Audio,
                    "sub" => TrackKind::Subtitle,
                    _ => return None,
                };
                Some(Track {
                    id: field("id")?.parse().ok()?,
                    kind,
                    lang: field("lang"),
                    title: field("title"),
                    codec: field("codec"),
                    selected: field("selected").as_deref() == Some("yes"),
                })
            })
            .collect()
    }

    /// Sceglie una traccia; per i sottotitoli `None` li spegne.
    pub fn select_track(&self, kind: TrackKind, id: Option<i64>) -> Result<(), PlayerError> {
        let value = id.map_or_else(|| "no".to_owned(), |id| id.to_string());
        match kind {
            TrackKind::Audio => self.set_property("traccia audio", "aid", &value),
            TrackKind::Subtitle => self.set_property("sottotitoli", "sid", &value),
        }
    }

    /// Ferma la riproduzione.
    pub fn stop(&self) -> Result<(), PlayerError> {
        self.command("stop", &["stop"])
    }

    /// Mette in pausa o riprende.
    pub fn set_paused(&self, paused: bool) -> Result<(), PlayerError> {
        self.set_property("pausa", "pause", if paused { "yes" } else { "no" })
    }

    /// In pausa adesso.
    pub fn is_paused(&self) -> bool {
        self.property("pause").as_deref() == Some("yes")
    }

    /// Posizione corrente in secondi (se c'è un flusso).
    pub fn position(&self) -> Option<f64> {
        self.property("time-pos")
            .and_then(|p| p.parse().ok())
            .filter(|p: &f64| p.is_finite())
    }

    /// Durata in secondi (assente per la TV in diretta).
    pub fn duration(&self) -> Option<f64> {
        self.property("duration")
            .and_then(|p| p.parse().ok())
            .filter(|d: &f64| d.is_finite() && *d > 0.0)
    }

    /// Salta avanti (positivo) o indietro (negativo) di `seconds`.
    pub fn seek_relative(&self, seconds: f64) -> Result<(), PlayerError> {
        self.command("salto", &["seek", &format!("{seconds:.1}"), "relative"])
    }

    /// Va al secondo `seconds` dall'inizio.
    pub fn seek_absolute(&self, seconds: f64) -> Result<(), PlayerError> {
        self.command(
            "salto",
            &["seek", &format!("{:.1}", seconds.max(0.0)), "absolute"],
        )
    }

    /// Imposta una proprietà di mpv (per esempio `video-aspect-override`).
    pub fn set_option(&self, name: &str, value: &str) -> Result<(), PlayerError> {
        self.set_property("impostazione", name, value)
    }

    /// Volume in percentuale (0-100, oltre amplifica).
    pub fn set_volume(&self, percent: u32) -> Result<(), PlayerError> {
        self.set_property("volume", "volume", &percent.to_string())
    }

    /// Riattiva la traccia video. mpv la disattiva quando l'uscita video si perde: se si apre un
    /// flusso prima che esista un [`RenderContext`], o quando il contesto passa da una finestra
    /// all'altra. Da chiamare dopo aver creato un nuovo `RenderContext`.
    pub fn reselect_video(&self) -> Result<(), PlayerError> {
        // Liberando il contesto mpv smonta la catena video ma può lasciare la traccia selezionata:
        // impostare solo "auto" sarebbe un no-op. Passando da "no" la catena si ricrea sempre.
        self.set_property("riattivazione del video", "vid", "no")?;
        self.set_property("riattivazione del video", "vid", "auto")
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // ferma subito l'audio: il core può sopravvivere ancora un po' se una finestra
        // tiene il RenderContext
        if let Err(err) = self.stop() {
            log::debug!("stop alla chiusura: {err}");
        }
        self.stop.store(true, Ordering::Release);
        // SAFETY: core valido; mpv_wakeup è thread-safe e sblocca mpv_wait_event.
        unsafe { ffi::mpv_wakeup(self.core.as_ptr()) };
        if let Some(events) = self.events.take() {
            if events.join().is_err() {
                log::error!("il thread degli eventi di mpv è terminato con un panic");
            }
        }
    }
}

/// Legge una stringa C che può essere NULL.
///
/// # Safety
/// `ptr` deve essere NULL o puntare a una stringa terminata da zero valida per tutta la durata `'a`.
unsafe fn opt_str<'a>(ptr: *const c_char) -> Option<std::borrow::Cow<'a, str>> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: garantito dal chiamante.
        Some(unsafe { CStr::from_ptr(ptr) }.to_string_lossy())
    }
}

fn event_loop<F>(core: &Core, stop: &AtomicBool, mut on_event: F)
where
    F: FnMut(PlayerEvent),
{
    loop {
        // SAFETY: core valido per tutta la vita del thread (Arc); solo questo thread chiama
        // mpv_wait_event, come richiesto da mpv.
        let event = unsafe { &*ffi::mpv_wait_event(core.as_ptr(), -1.0) };
        if stop.load(Ordering::Acquire) {
            break;
        }
        match event.event_id {
            ffi::MPV_EVENT_SHUTDOWN => break,
            ffi::MPV_EVENT_NONE => {}
            ffi::MPV_EVENT_LOG_MESSAGE => {
                // SAFETY: per questo tipo di evento `data` punta a un mpv_event_log_message,
                // valido fino alla prossima mpv_wait_event.
                let msg = unsafe { &*(event.data as *const ffi::mpv_event_log_message) };
                // SAFETY: stringhe di mpv valide fino alla prossima mpv_wait_event.
                let (prefix, level, text) =
                    unsafe { (opt_str(msg.prefix), opt_str(msg.level), opt_str(msg.text)) };
                let level = match level.as_deref() {
                    Some("fatal" | "error") => log::Level::Error,
                    Some("warn") => log::Level::Warn,
                    Some("info") => log::Level::Info,
                    Some("v" | "debug") => log::Level::Debug,
                    _ => log::Level::Trace,
                };
                let text = text.unwrap_or_default();
                log::log!(
                    target: "mpv",
                    level,
                    "[{}] {}",
                    prefix.unwrap_or_default(),
                    sintonia_core::redact::redact_text(text.trim_end())
                );
            }
            ffi::MPV_EVENT_START_FILE => on_event(PlayerEvent::Loading),
            ffi::MPV_EVENT_FILE_LOADED => on_event(PlayerEvent::Loaded),
            ffi::MPV_EVENT_PLAYBACK_RESTART => on_event(PlayerEvent::Playing),
            ffi::MPV_EVENT_IDLE => on_event(PlayerEvent::Idle),
            ffi::MPV_EVENT_END_FILE => {
                // SAFETY: per questo tipo di evento `data` punta a un mpv_event_end_file.
                let end = unsafe { &*(event.data as *const ffi::mpv_event_end_file) };
                let reason = match end.reason {
                    ffi::MPV_END_FILE_REASON_EOF => EndReason::Eof,
                    ffi::MPV_END_FILE_REASON_ERROR => {
                        let text = mpv_error("riproduzione", end.error).to_string();
                        EndReason::Error(text)
                    }
                    // REDIRECT (una playlist che ne apre un'altra) non è una fine vera
                    ffi::MPV_END_FILE_REASON_REDIRECT => continue,
                    _ => EndReason::Stopped,
                };
                on_event(PlayerEvent::Ended(reason));
            }
            ffi::MPV_EVENT_VIDEO_RECONFIG => {
                let size = (get_int(core, c"dwidth"), get_int(core, c"dheight"));
                if let (Some(width), Some(height)) = size {
                    on_event(PlayerEvent::VideoSize { width, height });
                }
            }
            ffi::MPV_EVENT_PROPERTY_CHANGE if event.reply_userdata == OBSERVE_PAUSED_FOR_CACHE => {
                // SAFETY: per PROPERTY_CHANGE `data` punta a un mpv_event_property.
                let prop = unsafe { &*(event.data as *const ffi::mpv_event_property) };
                if prop.format == ffi::MPV_FORMAT_FLAG && !prop.data.is_null() {
                    // SAFETY: con MPV_FORMAT_FLAG `data` punta a un int.
                    let flag = unsafe { *(prop.data as *const c_int) } != 0;
                    on_event(PlayerEvent::Buffering(flag));
                }
            }
            _ => {}
        }
    }
}

/// Legge una proprietà di mpv come stringa.
fn get_string(core: &Core, name: &CStr) -> Option<String> {
    // SAFETY: core valido; il risultato è NULL o una stringa allocata da mpv da liberare con mpv_free.
    let raw = unsafe { ffi::mpv_get_property_string(core.as_ptr(), name.as_ptr()) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: stringa valida restituita da mpv.
    let value = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: memoria allocata da mpv, liberata una sola volta.
    unsafe { ffi::mpv_free(raw.cast()) };
    Some(value)
}

/// Legge una proprietà intera di mpv.
fn get_int(core: &Core, name: &CStr) -> Option<u32> {
    get_string(core, name).and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn backoff_grows_then_gives_up() {
        let mut backoff = Backoff::default();
        let delays: Vec<u64> = std::iter::from_fn(|| backoff.next_delay())
            .map(|d| d.as_secs())
            .collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30, 30, 30]);
        assert_eq!(backoff.next_delay(), None);
        backoff.reset();
        assert_eq!(backoff.next_delay(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn per_file_options_are_length_quoted() {
        let options = LoadOptions {
            user_agent: Some("Mozilla/5.0 (a, b=c)".into()),
            referrer: Some("https://r.example/".into()),
            start_secs: Some(90),
        };
        assert_eq!(
            load_options_string(&options),
            "user-agent=%20%Mozilla/5.0 (a, b=c),referrer=%18%https://r.example/,start=90"
        );
        assert_eq!(load_options_string(&LoadOptions::default()), "");
    }

    #[test]
    fn load_options_debug_hides_values() {
        let options = LoadOptions {
            user_agent: Some("segreto".into()),
            ..LoadOptions::default()
        };
        assert!(!format!("{options:?}").contains("segreto"));
    }
}
