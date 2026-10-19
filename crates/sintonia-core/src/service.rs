//! Il servizio: tutto l'I/O di Sintonia (rete, disco, archivio, portachiavi) su un runtime tokio
//! in un thread a parte. L'interfaccia manda [`Command`] e riceve [`Event`]; non tocca mai rete o
//! disco.
//!
//! Gli eventi arrivano da thread del runtime attraverso la funzione passata a [`Service::start`]:
//! l'app li inoltra al suo thread (con Slint: `invoke_from_event_loop`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Semaphore};

use crate::crypto::Cipher;
use crate::db::{
    ChannelInput, ChannelRow, ChannelStream, EpgSummary, EpisodeRef, GuideProgramme, HistoryEntry,
    ItemKind, MovieRow, NewSource, SeriesRow, SourceInfo, SourceKind, Store, StoreError,
    StreamInfo,
};
use crate::epg::{self, EpgError, ParseStats, Programme, XmltvChannel};
use crate::m3u::{self, Parser, Playlist};
use crate::secrets::{source_password_name, SecretStore};
use crate::xtream::{self, AccountInfo, Credentials, MovieDetail, SeriesDetail, XtreamError};

/// Dimensione massima di una playlist (per non riempire la memoria con una risposta sbagliata).
const MAX_PLAYLIST_BYTES: u64 = 512 * 1024 * 1024;
/// Dimensione massima di un'immagine.
const MAX_IMAGE_BYTES: u64 = 6 * 1024 * 1024;
/// Immagini scaricate in parallelo.
const IMAGE_PARALLELISM: usize = 6;
/// Risultati massimi di una ricerca.
const SEARCH_LIMIT: u32 = 2_000;
/// Lato massimo in memoria: loghi (righe dell'elenco) e locandine (griglia dei film).
const LOGO_MAX_SIDE: u32 = 128;
const POSTER_MAX_SIDE: u32 = 360;
/// Voci di cronologia mandate all'interfaccia.
const HISTORY_LIMIT: u32 = 30;
/// Dimensione massima di una guida scaricata (anche compressa).
const MAX_EPG_BYTES: u64 = 1024 * 1024 * 1024;
/// Età oltre la quale la guida si aggiorna da sola (secondi).
const EPG_MAX_AGE: i64 = 12 * 3600;
/// Programmi tenuti: da un giorno fa a otto giorni da adesso (secondi).
const EPG_PAST: i64 = 86_400;
const EPG_FUTURE: i64 = 8 * 86_400;
/// Programmi scritti nell'archivio per volta (l'archivio resta libero tra un blocco e l'altro).
const EPG_BATCH: usize = 5_000;

/// Configurazione del servizio.
#[derive(Debug)]
pub struct ServiceConfig {
    /// File dell'archivio SQLite.
    pub database: PathBuf,
    /// Cartella della cache (immagini).
    pub cache_dir: PathBuf,
    /// Chiave dell'archivio.
    pub cipher: Cipher,
    /// Portachiavi per le password Xtream.
    pub secrets: Arc<dyn SecretStore>,
    /// User-agent predefinito per le richieste HTTP.
    pub user_agent: String,
}

/// Cosa aprire nel player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Playable {
    /// Un canale (M3U o Xtream).
    Channel(i64),
    /// Un film Xtream.
    Movie(i64),
    /// Un episodio di una serie Xtream (identificativo della serie nell'archivio).
    Episode {
        /// Serie.
        series_id: i64,
        /// Episodio.
        episode: EpisodeRef,
    },
}

/// Comandi dall'interfaccia.
#[derive(Debug, Clone)]
pub enum Command {
    /// Carica sorgenti, impostazioni, catalogo della sorgente in uso, preferiti e cronologia.
    Init,
    /// Scarica e legge una playlist senza salvarla (pulsante «Verifica»).
    VerifySource(NewSource),
    /// Scarica, legge e salva una sorgente M3U.
    AddSource(NewSource),
    /// Verifica un account Xtream (senza salvarlo).
    VerifyXtream {
        /// Server e utente.
        source: NewSource,
        /// Password.
        password: String,
    },
    /// Verifica, scarica il catalogo e salva un account Xtream.
    AddXtream {
        /// Server e utente.
        source: NewSource,
        /// Password (va nel portachiavi).
        password: String,
    },
    /// Riscarica il catalogo di una sorgente.
    RefreshSource(i64),
    /// Rende una sorgente quella in uso.
    ActivateSource(i64),
    /// Elimina una sorgente (e la sua password).
    DeleteSource(i64),
    /// Ricerca nella sorgente in uso (canali, film e serie); `generation` scarta i risultati
    /// superati.
    Search {
        /// Testo cercato.
        query: String,
        /// Numero crescente della richiesta.
        generation: u64,
    },
    /// Indirizzo di un contenuto da aprire.
    Resolve {
        /// Contenuto.
        item: Playable,
        /// Numero della richiesta (per ignorare risposte superate).
        token: u64,
    },
    /// Dettagli di un film.
    MovieDetail(i64),
    /// Stagioni ed episodi di una serie.
    SeriesDetail(i64),
    /// Un'immagine da scaricare (o leggere dalla cache): logo o locandina.
    FetchImage {
        /// Indirizzo.
        url: String,
        /// Locandina (più grande) invece di logo.
        poster: bool,
    },
    /// Aggiunge o toglie un preferito della sorgente in uso.
    SetFavorite {
        /// Tipo.
        kind: ItemKind,
        /// Chiave stabile.
        key: String,
        /// Preferito sì o no.
        favorite: bool,
    },
    /// Salva il punto di visione; con `notify` rimanda la cronologia all'interfaccia.
    SaveHistory {
        /// Voce.
        entry: HistoryEntry,
        /// Aggiornare l'interfaccia.
        notify: bool,
    },
    /// Toglie una voce dalla cronologia.
    RemoveHistory {
        /// Tipo.
        kind: ItemKind,
        /// Chiave.
        key: String,
    },
    /// Scarica e importa la guida dei programmi di una sorgente (`None`: quella in uso). Senza
    /// `force` solo se l'aggiornamento automatico è acceso e la guida è vecchia.
    RefreshEpg {
        /// Sorgente.
        source_id: Option<i64>,
        /// Anche se recente.
        force: bool,
    },
    /// Cambia la guida di una sorgente (indirizzo o file; `None` o vuoto: quella della
    /// playlist o del server) e la scarica subito.
    SetSourceEpg {
        /// Sorgente.
        source_id: i64,
        /// Indirizzo o percorso.
        epg_url: Option<String>,
    },
    /// Programmi della sorgente in uso tra `from` e `to` (secondi Unix).
    LoadGuide {
        /// Inizio.
        from: i64,
        /// Fine.
        to: i64,
        /// Numero della richiesta.
        generation: u64,
    },
    /// Salva un'impostazione.
    SetSetting {
        /// Chiave.
        key: String,
        /// Valore.
        value: String,
    },
}

/// Perché una sorgente non si è potuta leggere. L'interfaccia traduce questi casi in frasi.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// Risposta HTTP non riuscita.
    #[error("il server ha risposto {0}")]
    Http(u16),
    /// Server irraggiungibile, tempo scaduto, certificato non valido… (testo senza URL).
    #[error("rete: {0}")]
    Network(String),
    /// File non leggibile.
    #[error("file: {0}")]
    File(String),
    /// Il contenuto non contiene canali.
    #[error("nessun canale nella playlist")]
    Empty,
    /// Il contenuto supera il limite di dimensione.
    #[error("playlist troppo grande")]
    TooLarge,
    /// Errore dell'archivio.
    #[error("archivio: {0}")]
    Store(String),
    /// Utente o password non accettati.
    #[error("utente o password non accettati")]
    Auth,
    /// Indirizzo del server non valido.
    #[error("indirizzo del server non valido")]
    InvalidServer,
    /// Risposta del server non leggibile.
    #[error("risposta non valida: {0}")]
    Format(String),
    /// Portachiavi non disponibile.
    #[error("portachiavi: {0}")]
    Secrets(String),
}

impl From<StoreError> for SourceError {
    fn from(err: StoreError) -> Self {
        Self::Store(err.to_string())
    }
}

impl From<XtreamError> for SourceError {
    fn from(err: XtreamError) -> Self {
        match err {
            XtreamError::InvalidServer => Self::InvalidServer,
            XtreamError::Http(code) => Self::Http(code),
            XtreamError::Network(text) => Self::Network(text),
            XtreamError::Auth => Self::Auth,
            XtreamError::Json(text) => Self::Format(text),
            XtreamError::TooLarge => Self::TooLarge,
        }
    }
}

/// Esito della lettura di una sorgente.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceReport {
    /// Canali letti.
    pub channels: usize,
    /// Categorie distinte dei canali.
    pub groups: usize,
    /// Righe saltate (doppioni, indirizzi non validi…).
    pub skipped: usize,
    /// La playlist indica una guida dei programmi.
    pub has_epg: bool,
    /// Film (Xtream).
    pub movies: usize,
    /// Serie (Xtream).
    pub series: usize,
    /// Stato dell'account (Xtream).
    pub account: Option<AccountInfo>,
}

/// Un'immagine pronta da mostrare (RGBA, 8 bit per canale).
#[derive(Clone, PartialEq, Eq)]
pub struct LogoImage {
    /// Larghezza.
    pub width: u32,
    /// Altezza.
    pub height: u32,
    /// Pixel RGBA, riga per riga.
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for LogoImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LogoImage({}x{})", self.width, self.height)
    }
}

/// Un flusso pronto per il player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStream {
    /// Indirizzo e intestazioni.
    pub stream: StreamInfo,
    /// Contenuto on demand (film o episodio): finisce invece di riconnettersi.
    pub on_demand: bool,
}

/// Eventi verso l'interfaccia.
#[derive(Debug, Clone)]
pub enum Event {
    /// Impostazioni salvate (all'avvio).
    Settings(Vec<(String, String)>),
    /// Elenco delle sorgenti (dopo ogni modifica).
    Sources(Vec<SourceInfo>),
    /// Catalogo della sorgente in uso (`None`: nessuna sorgente).
    Catalog {
        /// Sorgente.
        source_id: Option<i64>,
        /// Canali in ordine.
        channels: Arc<Vec<ChannelRow>>,
        /// Film (vuoto per le sorgenti M3U).
        movies: Arc<Vec<MovieRow>>,
        /// Serie (vuoto per le sorgenti M3U).
        series: Arc<Vec<SeriesRow>>,
    },
    /// Preferiti della sorgente in uso.
    Favorites(Vec<(ItemKind, String)>),
    /// Cronologia della sorgente in uso, le più recenti prima.
    History(Vec<HistoryEntry>),
    /// Un'operazione lunga è iniziata o finita (download, lettura).
    Working(bool),
    /// Esito di «Verifica».
    Verified(Result<SourceReport, SourceError>),
    /// Esito di aggiunta o aggiornamento.
    SourceSaved {
        /// Sorgente (se salvata).
        source_id: Option<i64>,
        /// Esito.
        result: Result<SourceReport, SourceError>,
    },
    /// Risultati di una ricerca.
    SearchResults {
        /// Numero della richiesta.
        generation: u64,
        /// Canali trovati.
        channels: Vec<i64>,
        /// Film trovati.
        movies: Vec<i64>,
        /// Serie trovate.
        series: Vec<i64>,
    },
    /// Flusso da aprire.
    Stream {
        /// Numero della richiesta.
        token: u64,
        /// Esito.
        result: Result<ResolvedStream, String>,
    },
    /// Dettagli di un film.
    MovieDetail {
        /// Film.
        id: i64,
        /// Esito.
        result: Result<MovieDetail, String>,
    },
    /// Stagioni ed episodi di una serie.
    SeriesDetail {
        /// Serie.
        id: i64,
        /// Esito.
        result: Result<SeriesDetail, String>,
    },
    /// Immagine pronta (`None`: non disponibile).
    Image {
        /// Indirizzo richiesto.
        url: String,
        /// Locandina o logo.
        poster: bool,
        /// Immagine.
        image: Option<LogoImage>,
    },
    /// Aggiornamento della guida di una sorgente.
    Epg {
        /// Sorgente.
        source_id: i64,
        /// Stato.
        state: EpgState,
    },
    /// Programmi richiesti con [`Command::LoadGuide`].
    Guide {
        /// Sorgente in uso.
        source_id: Option<i64>,
        /// Inizio.
        from: i64,
        /// Fine.
        to: i64,
        /// Numero della richiesta.
        generation: u64,
        /// Programmi, per canale e in ordine di inizio.
        programmes: Arc<Vec<GuideProgramme>>,
    },
    /// Errore da mostrare.
    Error(String),
}

/// Stato dell'aggiornamento della guida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpgState {
    /// Download e importazione in corso.
    Working,
    /// Finito (con l'esito).
    Done(Result<EpgSummary, String>),
}

/// Errori di avvio del servizio.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// Archivio non apribile.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Runtime o client HTTP non avviabile.
    #[error("avvio del servizio: {0}")]
    Start(String),
    /// Cartella non creabile.
    #[error("cartella dei dati: {0}")]
    Io(#[from] std::io::Error),
}

/// Il servizio in esecuzione. Alla distruzione chiude il runtime.
#[derive(Debug)]
pub struct Service {
    commands: mpsc::UnboundedSender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
}

type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

struct Shared {
    store: Mutex<Store>,
    secrets: Arc<dyn SecretStore>,
    http: reqwest::Client,
    on_event: EventSink,
    cache_dir: PathBuf,
    images: Semaphore,
    images_in_flight: Mutex<HashSet<(String, bool)>>,
    /// Sorgenti di cui si sta importando la guida.
    epg_in_flight: Mutex<HashSet<i64>>,
}

impl Shared {
    fn emit(&self, event: Event) {
        (self.on_event)(event);
    }

    /// Esegue `f` sull'archivio in un thread bloccante del runtime.
    async fn with_store<T, F>(self: &Arc<Self>, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Store) -> Result<T, StoreError> + Send + 'static,
    {
        let shared = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let mut store = shared.store.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut store)
        })
        .await
        .unwrap_or(Err(StoreError::Interrupted))
    }

    /// Operazione sul portachiavi (può bloccare: thread a parte).
    async fn with_secrets<T, F>(self: &Arc<Self>, f: F) -> Result<T, SourceError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SecretStore) -> Result<T, crate::secrets::SecretsError> + Send + 'static,
    {
        let secrets = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || f(secrets.as_ref()))
            .await
            .map_err(|e| SourceError::Secrets(e.to_string()))?
            .map_err(|e| SourceError::Secrets(e.to_string()))
    }

    /// Client Xtream di una sorgente salvata (server e utente dall'archivio, password dal
    /// portachiavi).
    async fn xtream_client(
        self: &Arc<Self>,
        source_id: i64,
    ) -> Result<(xtream::Client, NewSource), SourceError> {
        let source = self
            .with_store(move |s| s.source_location(source_id))
            .await?;
        let password = self
            .with_secrets(move |s| s.get(&source_password_name(source_id)))
            .await?
            .ok_or_else(|| SourceError::Secrets("password non trovata".into()))?;
        let credentials = Credentials {
            server: xtream::normalize_server(&source.location)?,
            username: source.username.clone().unwrap_or_default(),
            password,
        };
        Ok((xtream::Client::new(self.http.clone(), credentials), source))
    }
}

impl Service {
    /// Apre l'archivio e avvia il runtime.
    pub fn start<F>(config: ServiceConfig, on_event: F) -> Result<Self, ServiceError>
    where
        F: Fn(Event) + Send + Sync + 'static,
    {
        if let Some(parent) = config.database.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(config.cache_dir.join("immagini"))?;
        let store = Store::open(&config.database, config.cipher)?;
        let http = reqwest::Client::builder()
            .user_agent(config.user_agent)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| ServiceError::Start(e.without_url().to_string()))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("sintonia-servizio")
            .enable_all()
            .build()
            .map_err(|e| ServiceError::Start(e.to_string()))?;

        let shared = Arc::new(Shared {
            store: Mutex::new(store),
            secrets: config.secrets,
            http,
            on_event: Arc::new(on_event),
            cache_dir: config.cache_dir,
            images: Semaphore::new(IMAGE_PARALLELISM),
            images_in_flight: Mutex::new(HashSet::new()),
            epg_in_flight: Mutex::new(HashSet::new()),
        });
        let (commands, mut inbox) = mpsc::unbounded_channel::<Command>();
        let thread = std::thread::Builder::new()
            .name("sintonia-servizio".into())
            .spawn(move || {
                runtime.block_on(async move {
                    while let Some(command) = inbox.recv().await {
                        let shared = Arc::clone(&shared);
                        tokio::spawn(async move { handle(shared, command).await });
                    }
                });
                // chiusura: le operazioni ancora in corso vengono abbandonate
                runtime.shutdown_timeout(Duration::from_secs(2));
            })
            .map_err(ServiceError::Io)?;
        Ok(Self {
            commands,
            thread: Some(thread),
        })
    }

    /// Manda un comando (non blocca).
    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            log::error!("il servizio non è più attivo");
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        // chiudere il canale fa uscire il ciclo dei comandi
        let (closed, _) = mpsc::unbounded_channel();
        drop(std::mem::replace(&mut self.commands, closed));
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                log::error!("il thread del servizio è terminato con un panic");
            }
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

async fn handle(shared: Arc<Shared>, command: Command) {
    match command {
        Command::Init => {
            match shared.with_store(|s| s.settings()).await {
                Ok(settings) => shared.emit(Event::Settings(settings)),
                Err(err) => shared.emit(Event::Error(err.to_string())),
            }
            emit_everything(&shared).await;
            refresh_epg(&shared, None, false).await;
        }
        Command::VerifySource(source) => {
            shared.emit(Event::Working(true));
            let result = load_playlist(&shared, &source).await.map(|p| report(&p));
            shared.emit(Event::Working(false));
            shared.emit(Event::Verified(result));
        }
        Command::AddSource(source) => {
            shared.emit(Event::Working(true));
            let result = add_m3u(&shared, source).await;
            finish_save(&shared, result).await;
        }
        Command::VerifyXtream { source, password } => {
            shared.emit(Event::Working(true));
            let result = verify_xtream(&shared, &source, password).await;
            shared.emit(Event::Working(false));
            shared.emit(Event::Verified(result));
        }
        Command::AddXtream { source, password } => {
            shared.emit(Event::Working(true));
            let result = add_xtream(&shared, source, password).await;
            finish_save(&shared, result).await;
        }
        Command::RefreshSource(id) => {
            shared.emit(Event::Working(true));
            let result = refresh_source(&shared, id).await.map(|r| (id, r));
            finish_save(&shared, result).await;
        }
        Command::ActivateSource(id) => {
            if let Err(err) = shared.with_store(move |s| s.set_active_source(id)).await {
                shared.emit(Event::Error(err.to_string()));
            }
            emit_everything(&shared).await;
            refresh_epg(&shared, Some(id), false).await;
        }
        Command::DeleteSource(id) => {
            if let Err(err) = shared.with_store(move |s| s.delete_source(id)).await {
                shared.emit(Event::Error(err.to_string()));
            }
            if let Err(err) = shared
                .with_secrets(move |s| s.delete(&source_password_name(id)))
                .await
            {
                log::warn!("password della sorgente non eliminata: {err}");
            }
            emit_everything(&shared).await;
        }
        Command::Search { query, generation } => {
            let result = shared
                .with_store(move |s| match s.active_source()? {
                    Some(source) => Ok((
                        s.search(source, &query, SEARCH_LIMIT)?,
                        s.search_movies(source, &query, SEARCH_LIMIT)?,
                        s.search_series(source, &query, SEARCH_LIMIT)?,
                    )),
                    None => Ok((Vec::new(), Vec::new(), Vec::new())),
                })
                .await;
            match result {
                Ok((channels, movies, series)) => shared.emit(Event::SearchResults {
                    generation,
                    channels,
                    movies,
                    series,
                }),
                Err(err) => shared.emit(Event::Error(err.to_string())),
            }
        }
        Command::Resolve { item, token } => {
            let result = resolve(&shared, item).await.map_err(|e| e.to_string());
            shared.emit(Event::Stream { token, result });
        }
        Command::MovieDetail(id) => {
            let result = async {
                let (source_id, movie) = shared.with_store(move |s| s.movie_ref(id)).await?;
                let (client, _) = shared.xtream_client(source_id).await?;
                Ok::<_, SourceError>(client.movie_detail(movie.xtream_id).await?)
            }
            .await
            .map_err(|e| e.to_string());
            shared.emit(Event::MovieDetail { id, result });
        }
        Command::SeriesDetail(id) => {
            let result = async {
                let (source_id, series) = shared.with_store(move |s| s.series_ref(id)).await?;
                let (client, _) = shared.xtream_client(source_id).await?;
                Ok::<_, SourceError>(client.series_detail(series.xtream_id).await?)
            }
            .await
            .map_err(|e| e.to_string());
            shared.emit(Event::SeriesDetail { id, result });
        }
        Command::FetchImage { url, poster } => fetch_image(&shared, url, poster).await,
        Command::SetFavorite {
            kind,
            key,
            favorite,
        } => {
            let result = shared
                .with_store(move |s| {
                    let Some(source) = s.active_source()? else {
                        return Ok(Vec::new());
                    };
                    s.set_favorite(source, kind, &key, favorite, now())?;
                    s.favorites(source)
                })
                .await;
            match result {
                Ok(favorites) => shared.emit(Event::Favorites(favorites)),
                Err(err) => shared.emit(Event::Error(err.to_string())),
            }
        }
        Command::SaveHistory { entry, notify } => {
            let result = shared
                .with_store(move |s| {
                    let Some(source) = s.active_source()? else {
                        return Ok(Vec::new());
                    };
                    s.save_history(source, &entry)?;
                    if notify {
                        s.history(source, HISTORY_LIMIT)
                    } else {
                        Ok(Vec::new())
                    }
                })
                .await;
            match result {
                Ok(history) if notify => shared.emit(Event::History(history)),
                Ok(_) => {}
                Err(err) => shared.emit(Event::Error(err.to_string())),
            }
        }
        Command::RemoveHistory { kind, key } => {
            let result = shared
                .with_store(move |s| {
                    let Some(source) = s.active_source()? else {
                        return Ok(Vec::new());
                    };
                    s.remove_history(source, kind, &key)?;
                    s.history(source, HISTORY_LIMIT)
                })
                .await;
            match result {
                Ok(history) => shared.emit(Event::History(history)),
                Err(err) => shared.emit(Event::Error(err.to_string())),
            }
        }
        Command::RefreshEpg { source_id, force } => refresh_epg(&shared, source_id, force).await,
        Command::SetSourceEpg { source_id, epg_url } => {
            let result = shared
                .with_store(move |s| {
                    s.set_epg_custom(source_id, epg_url.as_deref())?;
                    s.sources()
                })
                .await;
            match result {
                Ok(sources) => shared.emit(Event::Sources(sources)),
                Err(err) => shared.emit(Event::Error(err.to_string())),
            }
            refresh_epg(&shared, Some(source_id), true).await;
        }
        Command::LoadGuide {
            from,
            to,
            generation,
        } => {
            let result = shared
                .with_store(move |s| match s.active_source()? {
                    Some(id) => Ok((Some(id), s.guide(id, from, to)?)),
                    None => Ok((None, Vec::new())),
                })
                .await;
            match result {
                Ok((source_id, programmes)) => shared.emit(Event::Guide {
                    source_id,
                    from,
                    to,
                    generation,
                    programmes: Arc::new(programmes),
                }),
                Err(err) => shared.emit(Event::Error(err.to_string())),
            }
        }
        Command::SetSetting { key, value } => {
            if let Err(err) = shared
                .with_store(move |s| s.set_setting(&key, &value))
                .await
            {
                shared.emit(Event::Error(err.to_string()));
            }
        }
    }
}

async fn finish_save(shared: &Arc<Shared>, result: Result<(i64, SourceReport), SourceError>) {
    shared.emit(Event::Working(false));
    let (source_id, result) = match result {
        Ok((id, report)) => (Some(id), Ok(report)),
        Err(err) => (None, Err(err)),
    };
    shared.emit(Event::SourceSaved { source_id, result });
    emit_everything(shared).await;
    // sorgente nuova o appena aggiornata: anche la guida
    if let Some(id) = source_id {
        refresh_epg(shared, Some(id), true).await;
    }
}

// ───────────── guida dei programmi ─────────────

/// Aggiorna la guida di una sorgente, se ne ha una. Una sola importazione per sorgente alla
/// volta; l'esito arriva con [`Event::Epg`], poi le sorgenti (con i conteggi nuovi).
async fn refresh_epg(shared: &Arc<Shared>, source_id: Option<i64>, force: bool) {
    let id = match source_id {
        Some(id) => id,
        None => match shared.with_store(|s| s.active_source()).await {
            Ok(Some(id)) => id,
            _ => return,
        },
    };
    if !force {
        let check = shared
            .with_store(move |s| {
                Ok((
                    s.setting("epg_on_start")?.as_deref() != Some("0"),
                    s.epg_refreshed_at(id)?,
                ))
            })
            .await;
        match check {
            Ok((true, last)) if last.is_none_or(|t| now() - t >= EPG_MAX_AGE) => {}
            Ok(_) => return,
            Err(err) => {
                log::warn!("guida: {err}");
                return;
            }
        }
    }
    let locations = match epg_locations(shared, id).await {
        Ok(locations) if !locations.is_empty() => locations,
        Ok(_) => return,
        Err(err) => {
            shared.emit(Event::Epg {
                source_id: id,
                state: EpgState::Done(Err(err.to_string())),
            });
            return;
        }
    };
    if !shared
        .epg_in_flight
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(id)
    {
        return;
    }
    shared.emit(Event::Epg {
        source_id: id,
        state: EpgState::Working,
    });
    let result = import_epg(shared, id, &locations).await;
    shared
        .epg_in_flight
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&id);
    match &result {
        Ok(summary) => log::info!(
            "guida: {} programmi, {} canali abbinati",
            summary.programmes,
            summary.matched
        ),
        Err(err) => log::warn!("guida non aggiornata: {err}"),
    }
    shared.emit(Event::Epg {
        source_id: id,
        state: EpgState::Done(result.map_err(|e| e.to_string())),
    });
    if let Ok(sources) = shared.with_store(|s| s.sources()).await {
        shared.emit(Event::Sources(sources));
    }
}

/// Da dove leggere la guida: quella scelta dall'utente, altrimenti quella del server Xtream o
/// quelle indicate dalla playlist. Gli indirizzi possono contenere credenziali: mai nei log.
async fn epg_locations(shared: &Arc<Shared>, id: i64) -> Result<Vec<String>, SourceError> {
    let source = shared.with_store(move |s| s.source_location(id)).await?;
    if let Some(custom) = source.epg_url.filter(|u| !u.trim().is_empty()) {
        return Ok(vec![custom.trim().to_owned()]);
    }
    if source.kind == SourceKind::Xtream {
        let (client, _) = shared.xtream_client(id).await?;
        return Ok(vec![client.xmltv_url()]);
    }
    Ok(shared.with_store(move |s| s.epg_urls(id)).await?)
}

async fn import_epg(
    shared: &Arc<Shared>,
    id: i64,
    locations: &[String],
) -> Result<EpgSummary, SourceError> {
    shared.with_store(move |s| s.epg_begin(id)).await?;
    let window = (now() - EPG_PAST, now() + EPG_FUTURE);
    let mut imported = false;
    let mut last_error = None;
    for (n, location) in locations.iter().enumerate() {
        let remote = location.starts_with("http://") || location.starts_with("https://");
        let path = if remote {
            let path = shared.cache_dir.join(format!("guida-{id}-{n}.tmp"));
            if let Err(err) = download_to_file(&shared.http, location, &path).await {
                let _ = tokio::fs::remove_file(&path).await;
                last_error = Some(err);
                continue;
            }
            path
        } else {
            PathBuf::from(location)
        };
        let worker = Arc::clone(shared);
        let file = path.clone();
        let result = tokio::task::spawn_blocking(move || import_file(&worker, id, &file, window))
            .await
            .unwrap_or_else(|e| Err(SourceError::File(e.to_string())));
        if remote {
            let _ = tokio::fs::remove_file(&path).await;
        }
        match result {
            Ok(stats) if stats.programmes > 0 => imported = true,
            Ok(_) => last_error = Some(SourceError::Empty),
            Err(err) => last_error = Some(err),
        }
    }
    if !imported {
        let _ = shared.with_store(move |s| s.epg_abort(id)).await;
        return Err(last_error.unwrap_or(SourceError::Empty));
    }
    Ok(shared.with_store(move |s| s.epg_commit(id, now())).await?)
}

/// Scarica un file sul disco, a flusso e con un limite di dimensione.
async fn download_to_file(
    http: &reqwest::Client,
    url: &str,
    path: &Path,
) -> Result<(), SourceError> {
    let network = |e: reqwest::Error| SourceError::Network(e.without_url().to_string());
    let response = http.get(url).send().await.map_err(network)?;
    if !response.status().is_success() {
        return Err(SourceError::Http(response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_EPG_BYTES)
    {
        return Err(SourceError::TooLarge);
    }
    let file_error = |e: std::io::Error| SourceError::File(e.to_string());
    let mut file = tokio::fs::File::create(path).await.map_err(file_error)?;
    let mut total: u64 = 0;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(network)?;
        total += chunk.len() as u64;
        if total > MAX_EPG_BYTES {
            return Err(SourceError::TooLarge);
        }
        file.write_all(&chunk).await.map_err(file_error)?;
    }
    file.flush().await.map_err(file_error)?;
    Ok(())
}

/// Blocco in preparazione durante la lettura di una guida.
#[derive(Default)]
struct EpgBatch {
    channels: Vec<XmltvChannel>,
    programmes: Vec<Programme>,
    failure: Option<StoreError>,
}

impl EpgBatch {
    fn flush(&mut self, shared: &Shared, id: i64) {
        if self.failure.is_some() || (self.channels.is_empty() && self.programmes.is_empty()) {
            return;
        }
        let mut store = shared.store.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(err) = store.epg_insert(id, &self.channels, &self.programmes) {
            self.failure = Some(err);
        }
        self.channels.clear();
        self.programmes.clear();
    }
}

/// Legge un file della guida (thread bloccante) e lo scrive nell'archivio a blocchi.
fn import_file(
    shared: &Shared,
    id: i64,
    path: &Path,
    window: (i64, i64),
) -> Result<ParseStats, SourceError> {
    let file = std::fs::File::open(path).map_err(|e| SourceError::File(e.to_string()))?;
    let reader = epg::open(file).map_err(|e| SourceError::File(e.to_string()))?;
    let batch = RefCell::new(EpgBatch::default());
    let stats = epg::parse(
        reader,
        window,
        &mut |channel| batch.borrow_mut().channels.push(channel),
        &mut |programme| {
            let mut batch = batch.borrow_mut();
            batch.programmes.push(programme);
            if batch.programmes.len() >= EPG_BATCH {
                batch.flush(shared, id);
            }
        },
    )
    .map_err(|e| match e {
        EpgError::Io(text) => SourceError::File(text),
        EpgError::Xml(text) => SourceError::Format(text),
    })?;
    let mut batch = batch.into_inner();
    batch.flush(shared, id);
    match batch.failure {
        Some(err) => Err(err.into()),
        None => Ok(stats),
    }
}

/// Sorgenti, catalogo della sorgente in uso, preferiti e cronologia.
async fn emit_everything(shared: &Arc<Shared>) {
    let result = shared
        .with_store(|s| {
            let sources = s.sources()?;
            let active = s.active_source()?;
            let (channels, movies, series, favorites, history) = match active {
                Some(id) => (
                    s.channels(id)?,
                    s.movies(id)?,
                    s.series(id)?,
                    s.favorites(id)?,
                    s.history(id, HISTORY_LIMIT)?,
                ),
                None => Default::default(),
            };
            Ok((
                sources, active, channels, movies, series, favorites, history,
            ))
        })
        .await;
    match result {
        Ok((sources, source_id, channels, movies, series, favorites, history)) => {
            shared.emit(Event::Sources(sources));
            shared.emit(Event::Catalog {
                source_id,
                channels: Arc::new(channels),
                movies: Arc::new(movies),
                series: Arc::new(series),
            });
            shared.emit(Event::Favorites(favorites));
            shared.emit(Event::History(history));
        }
        Err(err) => shared.emit(Event::Error(err.to_string())),
    }
}

fn report(playlist: &Playlist) -> SourceReport {
    let groups: HashSet<&str> = playlist
        .entries
        .iter()
        .map(|e| e.group.as_deref().unwrap_or_default())
        .collect();
    SourceReport {
        channels: playlist.entries.len(),
        groups: groups.len(),
        skipped: m3u::count_skipped(&playlist.warnings),
        has_epg: !playlist.epg_urls.is_empty(),
        ..SourceReport::default()
    }
}

async fn add_m3u(
    shared: &Arc<Shared>,
    source: NewSource,
) -> Result<(i64, SourceReport), SourceError> {
    let playlist = load_playlist(shared, &source).await?;
    let report = report(&playlist);
    let id = shared
        .with_store(move |s| {
            let id = s.add_source(&source)?;
            s.replace_channels(id, &playlist.entries, &playlist.epg_urls, now())?;
            Ok(id)
        })
        .await?;
    Ok((id, report))
}

async fn refresh_source(shared: &Arc<Shared>, id: i64) -> Result<SourceReport, SourceError> {
    let source = shared.with_store(move |s| s.source_location(id)).await?;
    if source.kind == SourceKind::Xtream {
        let (client, _) = shared.xtream_client(id).await?;
        let catalog = load_xtream_catalog(&client).await?;
        return save_xtream_catalog(shared, id, catalog).await;
    }
    let playlist = load_playlist(shared, &source).await?;
    let report = report(&playlist);
    shared
        .with_store(move |s| s.replace_channels(id, &playlist.entries, &playlist.epg_urls, now()))
        .await?;
    Ok(report)
}

// ───────────── Xtream ─────────────

fn xtream_client_for(
    shared: &Shared,
    source: &NewSource,
    password: String,
) -> Result<xtream::Client, SourceError> {
    let credentials = Credentials {
        server: xtream::normalize_server(&source.location)?,
        username: source.username.clone().unwrap_or_default(),
        password,
    };
    Ok(xtream::Client::new(shared.http.clone(), credentials))
}

async fn verify_xtream(
    shared: &Arc<Shared>,
    source: &NewSource,
    password: String,
) -> Result<SourceReport, SourceError> {
    let client = xtream_client_for(shared, source, password)?;
    let catalog = load_xtream_catalog(&client).await?;
    Ok(catalog.report())
}

async fn add_xtream(
    shared: &Arc<Shared>,
    mut source: NewSource,
    password: String,
) -> Result<(i64, SourceReport), SourceError> {
    source.location = xtream::normalize_server(&source.location)?;
    let client = xtream_client_for(shared, &source, password.clone())?;
    let catalog = load_xtream_catalog(&client).await?;
    let id = shared.with_store(move |s| s.add_source(&source)).await?;
    if let Err(err) = shared
        .with_secrets(move |s| s.set(&source_password_name(id), &password))
        .await
    {
        // senza password la sorgente non servirebbe: la si toglie
        let _ = shared.with_store(move |s| s.delete_source(id)).await;
        return Err(err);
    }
    let report = save_xtream_catalog(shared, id, catalog).await?;
    Ok((id, report))
}

/// Il catalogo di un account Xtream, già pronto per l'archivio.
struct XtreamCatalog {
    account: AccountInfo,
    channels: Vec<ChannelInput>,
    movies: Vec<MovieRow>,
    series: Vec<SeriesRow>,
    groups: usize,
}

impl XtreamCatalog {
    fn report(&self) -> SourceReport {
        SourceReport {
            channels: self.channels.len(),
            groups: self.groups,
            movies: self.movies.len(),
            series: self.series.len(),
            account: Some(self.account.clone()),
            ..SourceReport::default()
        }
    }
}

fn category_names(categories: Vec<xtream::Category>) -> HashMap<String, String> {
    categories
        .into_iter()
        .filter_map(|c| Some((c.category_id?, c.category_name.unwrap_or_default())))
        .collect()
}

/// Anno dai primi quattro caratteri di una data, se sono un anno plausibile.
fn year_of(date: Option<&str>) -> Option<String> {
    let year = date?.trim().get(..4)?;
    (year.chars().all(|c| c.is_ascii_digit()) && year > "1880").then(|| year.to_owned())
}

async fn load_xtream_catalog(client: &xtream::Client) -> Result<XtreamCatalog, SourceError> {
    let account = client.account().await?;
    let (live_cats, live, movie_cats, movies, series_cats, series) = tokio::try_join!(
        client.live_categories(),
        client.live_streams(),
        client.movie_categories(),
        client.movies(),
        client.series_categories(),
        client.series(),
    )?;
    let (live_cats, movie_cats, series_cats) = (
        category_names(live_cats),
        category_names(movie_cats),
        category_names(series_cats),
    );
    let group = |names: &HashMap<String, String>, id: &Option<String>| {
        id.as_ref()
            .and_then(|id| names.get(id))
            .cloned()
            .unwrap_or_default()
    };
    let channels: Vec<ChannelInput> = live
        .into_iter()
        .enumerate()
        .filter_map(|(position, s)| {
            let id = s.stream_id?;
            Some(ChannelInput {
                key: format!("l{id}"),
                number: s
                    .num
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .unwrap_or_else(|| u32::try_from(position + 1).unwrap_or(u32::MAX)),
                name: s.name.unwrap_or_else(|| format!("#{id}")),
                group: group(&live_cats, &s.category_id),
                tvg_id: s.epg_channel_id,
                logo: s.stream_icon,
                xtream_id: Some(id),
                ..ChannelInput::default()
            })
        })
        .collect();
    let groups = channels
        .iter()
        .map(|c| c.group.as_str())
        .collect::<HashSet<_>>()
        .len();
    let movies = movies
        .into_iter()
        .filter_map(|m| {
            Some(MovieRow {
                xtream_id: m.stream_id?,
                name: m.name.unwrap_or_default(),
                group: group(&movie_cats, &m.category_id),
                cover: m.stream_icon,
                rating: m.rating.filter(|r| *r > 0.0),
                year: year_of(m.year.as_deref()),
                added: m.added,
                container: m.container_extension,
                ..MovieRow::default()
            })
        })
        .collect();
    let series = series
        .into_iter()
        .filter_map(|s| {
            Some(SeriesRow {
                xtream_id: s.series_id?,
                name: s.name.unwrap_or_default(),
                group: group(&series_cats, &s.category_id),
                cover: s.cover,
                rating: s.rating.filter(|r| *r > 0.0),
                year: year_of(s.release_date.as_deref()),
                plot: s.plot,
                genre: s.genre,
                ..SeriesRow::default()
            })
        })
        .collect();
    Ok(XtreamCatalog {
        account,
        channels,
        movies,
        series,
        groups,
    })
}

async fn save_xtream_catalog(
    shared: &Arc<Shared>,
    id: i64,
    catalog: XtreamCatalog,
) -> Result<SourceReport, SourceError> {
    let report = catalog.report();
    let ext = catalog.account.live_extension().to_owned();
    let expires = catalog.account.expires;
    shared
        .with_store(move |s| {
            s.replace_catalog(
                id,
                &catalog.channels,
                Some((&catalog.movies, &catalog.series)),
                &[],
                now(),
            )?;
            s.set_account_info(id, &ext, expires)
        })
        .await?;
    Ok(report)
}

/// Indirizzo da aprire per un contenuto.
async fn resolve(shared: &Arc<Shared>, item: Playable) -> Result<ResolvedStream, SourceError> {
    match item {
        Playable::Channel(channel_id) => {
            match shared
                .with_store(move |s| s.channel_stream(channel_id))
                .await?
            {
                ChannelStream::Url(stream) => Ok(ResolvedStream {
                    stream,
                    on_demand: false,
                }),
                ChannelStream::Xtream {
                    source_id,
                    xtream_id,
                } => {
                    let ext = shared
                        .with_store(move |s| s.live_extension(source_id))
                        .await?;
                    let (client, source) = shared.xtream_client(source_id).await?;
                    Ok(ResolvedStream {
                        stream: StreamInfo {
                            url: client.live_url(xtream_id, &ext),
                            user_agent: source.user_agent,
                            referrer: None,
                        },
                        on_demand: false,
                    })
                }
            }
        }
        Playable::Movie(movie_id) => {
            let (source_id, movie) = shared.with_store(move |s| s.movie_ref(movie_id)).await?;
            let (client, source) = shared.xtream_client(source_id).await?;
            let container = movie.container.as_deref().unwrap_or("mp4");
            Ok(ResolvedStream {
                stream: StreamInfo {
                    url: client.movie_url(movie.xtream_id, container),
                    user_agent: source.user_agent,
                    referrer: None,
                },
                on_demand: true,
            })
        }
        Playable::Episode { series_id, episode } => {
            let (source_id, _) = shared.with_store(move |s| s.series_ref(series_id)).await?;
            let (client, source) = shared.xtream_client(source_id).await?;
            Ok(ResolvedStream {
                stream: StreamInfo {
                    url: client.episode_url(episode.id, &episode.container),
                    user_agent: source.user_agent,
                    referrer: None,
                },
                on_demand: true,
            })
        }
    }
}

// ───────────── M3U ─────────────

/// Scarica (o legge dal disco) e interpreta una playlist, riga per riga mentre arriva.
async fn load_playlist(shared: &Shared, source: &NewSource) -> Result<Playlist, SourceError> {
    let playlist = match source.kind {
        SourceKind::M3uFile => {
            let path = PathBuf::from(&source.location);
            tokio::task::spawn_blocking(move || {
                let file =
                    std::fs::File::open(&path).map_err(|e| SourceError::File(e.to_string()))?;
                let size = file.metadata().map(|m| m.len()).unwrap_or(0);
                if size > MAX_PLAYLIST_BYTES {
                    return Err(SourceError::TooLarge);
                }
                m3u::parse_reader(BufReader::new(file))
                    .map_err(|e| SourceError::File(e.to_string()))
            })
            .await
            .map_err(|e| SourceError::File(e.to_string()))??
        }
        SourceKind::M3uUrl => download_playlist(&shared.http, source).await?,
        SourceKind::Xtream => return Err(SourceError::InvalidServer),
    };
    if playlist.entries.is_empty() {
        return Err(SourceError::Empty);
    }
    Ok(playlist)
}

async fn download_playlist(
    http: &reqwest::Client,
    source: &NewSource,
) -> Result<Playlist, SourceError> {
    let network = |e: reqwest::Error| SourceError::Network(e.without_url().to_string());
    let mut request = http.get(&source.location);
    if let Some(ua) = &source.user_agent {
        request = request.header(reqwest::header::USER_AGENT, ua);
    }
    let response = request.send().await.map_err(network)?;
    if !response.status().is_success() {
        return Err(SourceError::Http(response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_PLAYLIST_BYTES)
    {
        return Err(SourceError::TooLarge);
    }
    let mut parser = Parser::new();
    let mut entries = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut total: u64 = 0;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(network)?;
        total += chunk.len() as u64;
        if total > MAX_PLAYLIST_BYTES {
            return Err(SourceError::TooLarge);
        }
        pending.extend_from_slice(&chunk);
        let mut start = 0;
        while let Some(pos) = pending[start..].iter().position(|b| *b == b'\n') {
            let line = String::from_utf8_lossy(&pending[start..start + pos]);
            if let Some(entry) = parser.push_line(&line) {
                entries.push(entry);
            }
            start += pos + 1;
        }
        pending.drain(..start);
    }
    if !pending.is_empty() {
        if let Some(entry) = parser.push_line(&String::from_utf8_lossy(&pending)) {
            entries.push(entry);
        }
    }
    Ok(Playlist {
        entries,
        ..parser.finish()
    })
}

// ───────────── immagini ─────────────

/// Nome del file di cache di un'immagine: hash dell'indirizzo (non si salvano URL su disco).
fn image_cache_name(url: &str) -> String {
    Sha256::digest(url.as_bytes())
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn fetch_image(shared: &Arc<Shared>, url: String, poster: bool) {
    {
        let mut in_flight = shared
            .images_in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !in_flight.insert((url.clone(), poster)) {
            return;
        }
    }
    let image = load_image(shared, &url, poster).await;
    shared
        .images_in_flight
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&(url.clone(), poster));
    shared.emit(Event::Image { url, poster, image });
}

async fn load_image(shared: &Arc<Shared>, url: &str, poster: bool) -> Option<LogoImage> {
    let path = shared
        .cache_dir
        .join("immagini")
        .join(image_cache_name(url));
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(_) => {
            let _permit = shared.images.acquire().await.ok()?;
            let bytes = download_image(&shared.http, url).await?;
            if let Err(err) = tokio::fs::write(&path, &bytes).await {
                log::debug!("cache dell'immagine non scritta: {err}");
            }
            bytes
        }
    };
    let max_side = if poster {
        POSTER_MAX_SIDE
    } else {
        LOGO_MAX_SIDE
    };
    tokio::task::spawn_blocking(move || decode_image(&bytes, max_side))
        .await
        .ok()
        .flatten()
}

async fn download_image(http: &reqwest::Client, url: &str) -> Option<Vec<u8>> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let response = http
        .get(url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .ok()?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|len| len > MAX_IMAGE_BYTES)
    {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    (bytes.len() as u64 <= MAX_IMAGE_BYTES).then(|| bytes.to_vec())
}

fn decode_image(bytes: &[u8], max_side: u32) -> Option<LogoImage> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width() > max_side || image.height() > max_side {
        image.thumbnail(max_side, max_side)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Some(LogoImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecrets;
    use std::sync::mpsc as std_mpsc;

    fn start(dir: &std::path::Path) -> (Service, std_mpsc::Receiver<Event>, Arc<MemorySecrets>) {
        let (tx, rx) = std_mpsc::channel();
        let tx = Mutex::new(tx);
        let secrets = Arc::new(MemorySecrets::default());
        let service = Service::start(
            ServiceConfig {
                database: dir.join("archivio.db"),
                cache_dir: dir.join("cache"),
                cipher: Cipher::new(&[3; 32]),
                secrets: Arc::clone(&secrets) as Arc<dyn SecretStore>,
                user_agent: "Sintonia-test".into(),
            },
            move |event| {
                let _ = tx.lock().unwrap().send(event);
            },
        )
        .unwrap();
        (service, rx, secrets)
    }

    fn wait<T>(rx: &std_mpsc::Receiver<Event>, pick: impl Fn(Event) -> Option<T>) -> T {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let event = rx.recv_timeout(left).expect("evento atteso non arrivato");
            if let Some(value) = pick(event) {
                return value;
            }
        }
    }

    type Catalog = (
        Arc<Vec<ChannelRow>>,
        Arc<Vec<MovieRow>>,
        Arc<Vec<SeriesRow>>,
    );

    fn catalog(rx: &std_mpsc::Receiver<Event>) -> Catalog {
        wait(rx, |e| match e {
            Event::Catalog {
                channels,
                movies,
                series,
                ..
            } => Some((channels, movies, series)),
            _ => None,
        })
    }

    #[test]
    fn add_file_source_then_search_resolve_and_favorites() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lista.m3u");
        std::fs::write(
            &file,
            "#EXTM3U\n#EXTINF:-1 group-title=\"News\",Notizie Uno\nhttp://h/u/p/1.ts\n\
             #EXTINF:-1 group-title=\"Sport\",Sport Due\nhttp://h/u/p/2.ts\n",
        )
        .unwrap();
        let (service, rx, _) = start(dir.path());
        service.send(Command::AddSource(NewSource {
            name: "Prova".into(),
            kind: SourceKind::M3uFile,
            location: file.to_string_lossy().into_owned(),
            username: None,
            user_agent: None,
            epg_url: None,
        }));
        let saved = wait(&rx, |e| match e {
            Event::SourceSaved { result, .. } => Some(result),
            _ => None,
        });
        assert_eq!(saved.unwrap().channels, 2);
        let (channels, movies, _) = catalog(&rx);
        assert_eq!(channels.len(), 2);
        assert!(movies.is_empty());

        service.send(Command::Search {
            query: "sport".into(),
            generation: 7,
        });
        let (generation, ids) = wait(&rx, |e| match e {
            Event::SearchResults {
                generation,
                channels,
                ..
            } => Some((generation, channels)),
            _ => None,
        });
        assert_eq!((generation, ids), (7, vec![channels[1].id]));

        service.send(Command::Resolve {
            item: Playable::Channel(channels[0].id),
            token: 1,
        });
        let stream = wait(&rx, |e| match e {
            Event::Stream { result, .. } => Some(result),
            _ => None,
        })
        .unwrap();
        assert_eq!(stream.stream.url, "http://h/u/p/1.ts");
        assert!(!stream.on_demand);

        service.send(Command::SetFavorite {
            kind: ItemKind::Live,
            key: channels[1].key.clone(),
            favorite: true,
        });
        let favorites = wait(&rx, |e| match e {
            Event::Favorites(f) if !f.is_empty() => Some(f),
            _ => None,
        });
        assert_eq!(favorites, [(ItemKind::Live, channels[1].key.clone())]);
    }

    #[test]
    fn missing_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (service, rx, _) = start(dir.path());
        service.send(Command::VerifySource(NewSource {
            name: "X".into(),
            kind: SourceKind::M3uFile,
            location: dir
                .path()
                .join("non-esiste.m3u")
                .to_string_lossy()
                .into_owned(),
            username: None,
            user_agent: None,
            epg_url: None,
        }));
        let result = wait(&rx, |e| match e {
            Event::Verified(result) => Some(result),
            _ => None,
        });
        assert!(matches!(result, Err(SourceError::File(_))));
    }

    #[test]
    fn image_names_do_not_contain_the_url() {
        let name = image_cache_name("http://h/u/p/logo.png");
        assert_eq!(name.len(), 32);
        assert!(!name.contains("logo"));
    }

    #[test]
    fn images_are_decoded_and_shrunk() {
        let mut png = Vec::new();
        image::RgbaImage::new(400, 200)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let logo = decode_image(&png, LOGO_MAX_SIDE).unwrap();
        assert_eq!((logo.width, logo.height), (128, 64));
        assert_eq!(logo.rgba.len(), 128 * 64 * 4);
        let poster = decode_image(&png, POSTER_MAX_SIDE).unwrap();
        assert_eq!((poster.width, poster.height), (360, 180));
        assert!(decode_image(b"non un'immagine", LOGO_MAX_SIDE).is_none());
    }

    #[test]
    fn years_from_dates() {
        assert_eq!(year_of(Some("2021-05-01")), Some("2021".into()));
        assert_eq!(year_of(Some("n/d")), None);
        assert_eq!(year_of(Some("0000-00-00")), None);
        assert_eq!(year_of(None), None);
    }

    #[cfg(feature = "mock")]
    #[test]
    fn xtream_account_end_to_end() {
        let server = crate::xtream_mock::MockServer::start(crate::xtream_mock::Catalog::sample());
        let dir = tempfile::tempdir().unwrap();
        let (service, rx, secrets) = start(dir.path());
        let source = NewSource {
            name: "Casa".into(),
            kind: SourceKind::Xtream,
            location: server.address(),
            username: Some("mario".into()),
            user_agent: None,
            epg_url: None,
        };
        // password sbagliata
        service.send(Command::VerifyXtream {
            source: source.clone(),
            password: "no".into(),
        });
        let wrong = wait(&rx, |e| match e {
            Event::Verified(r) => Some(r),
            _ => None,
        });
        assert_eq!(wrong, Err(SourceError::Auth));

        service.send(Command::AddXtream {
            source,
            password: crate::xtream_mock::PASSWORD.into(),
        });
        let report = wait(&rx, |e| match e {
            Event::SourceSaved { result, .. } => Some(result),
            _ => None,
        })
        .unwrap();
        assert!(report.account.as_ref().is_some_and(AccountInfo::is_active));
        let (channels, movies, series) = catalog(&rx);
        assert_eq!(channels.len(), report.channels);
        assert!(!movies.is_empty() && !series.is_empty());
        assert!(
            !channels[0].group.is_empty(),
            "nome della categoria risolto"
        );
        // la password sta nel portachiavi, non nell'archivio
        let source_id = 1;
        assert_eq!(
            secrets
                .get(&source_password_name(source_id))
                .unwrap()
                .as_deref(),
            Some(crate::xtream_mock::PASSWORD)
        );

        service.send(Command::Resolve {
            item: Playable::Movie(movies[0].id),
            token: 5,
        });
        let movie = wait(&rx, |e| match e {
            Event::Stream { token: 5, result } => Some(result),
            _ => None,
        })
        .unwrap();
        assert!(movie.on_demand);
        assert!(movie.stream.url.contains("/movie/mario/"));

        service.send(Command::SeriesDetail(series[0].id));
        let detail = wait(&rx, |e| match e {
            Event::SeriesDetail { result, .. } => Some(result),
            _ => None,
        })
        .unwrap();
        assert!(!detail.episodes.is_empty());

        // la guida dell'account arriva da sola dopo il salvataggio (xmltv.php)
        let summary = wait(&rx, |e| match e {
            Event::Epg {
                state: EpgState::Done(result),
                ..
            } => Some(result),
            _ => None,
        })
        .unwrap();
        assert_eq!(summary.matched as usize, channels.len(), "{summary:?}");
        service.send(Command::LoadGuide {
            from: now(),
            to: now() + 3600,
            generation: 9,
        });
        let guide = wait(&rx, |e| match e {
            Event::Guide {
                generation: 9,
                programmes,
                ..
            } => Some(programmes),
            _ => None,
        });
        let first_key = &channels[0].key;
        assert!(
            guide
                .iter()
                .any(|p| &p.channel_key == first_key && p.start <= now()),
            "programma in onda sul primo canale"
        );
        // già aggiornata: senza «force» non si riscarica
        service.send(Command::RefreshEpg {
            source_id: Some(source_id),
            force: false,
        });
        service.send(Command::LoadGuide {
            from: 0,
            to: 1,
            generation: 10,
        });
        wait(&rx, |e| match e {
            Event::Epg { .. } => panic!("guida riscaricata senza motivo"),
            Event::Guide { generation: 10, .. } => Some(()),
            _ => None,
        });

        service.send(Command::DeleteSource(source_id));
        let _ = catalog(&rx);
        assert_eq!(secrets.get(&source_password_name(source_id)).unwrap(), None);
    }
}
