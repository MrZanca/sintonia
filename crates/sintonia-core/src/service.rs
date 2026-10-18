//! Il servizio: tutto l'I/O di Sintonia (rete, disco, archivio) su un runtime tokio in un thread a
//! parte. L'interfaccia manda [`Command`] e riceve [`Event`]; non tocca mai rete o disco.
//!
//! Gli eventi arrivano da thread del runtime attraverso la funzione passata a [`Service::start`]:
//! l'app li inoltra al suo thread (con Slint: `invoke_from_event_loop`).

use std::collections::HashSet;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, Semaphore};

use crate::crypto::Cipher;
use crate::db::{ChannelRow, NewSource, SourceInfo, SourceKind, Store, StoreError, StreamInfo};
use crate::m3u::{self, Parser, Playlist};

/// Dimensione massima di una playlist (per non riempire la memoria con una risposta sbagliata).
const MAX_PLAYLIST_BYTES: u64 = 512 * 1024 * 1024;
/// Dimensione massima di un logo.
const MAX_LOGO_BYTES: u64 = 4 * 1024 * 1024;
/// Loghi scaricati in parallelo.
const LOGO_PARALLELISM: usize = 6;
/// Risultati massimi di una ricerca.
const SEARCH_LIMIT: u32 = 2_000;
/// Lato massimo dei loghi in memoria (pixel): bastano per le righe dell'elenco.
const LOGO_MAX_SIDE: u32 = 128;

/// Configurazione del servizio.
#[derive(Debug)]
pub struct ServiceConfig {
    /// File dell'archivio SQLite.
    pub database: PathBuf,
    /// Cartella della cache (loghi).
    pub cache_dir: PathBuf,
    /// Chiave dell'archivio.
    pub cipher: Cipher,
    /// User-agent predefinito per le richieste HTTP.
    pub user_agent: String,
}

/// Comandi dall'interfaccia.
#[derive(Debug, Clone)]
pub enum Command {
    /// Carica sorgenti, impostazioni e canali della sorgente in uso.
    Init,
    /// Scarica e legge una playlist senza salvarla (pulsante «Verifica»).
    VerifySource(NewSource),
    /// Scarica, legge e salva una sorgente.
    AddSource(NewSource),
    /// Riscarica i canali di una sorgente.
    RefreshSource(i64),
    /// Rende una sorgente quella in uso.
    ActivateSource(i64),
    /// Elimina una sorgente.
    DeleteSource(i64),
    /// Ricerca nella sorgente in uso; `generation` permette di scartare risultati superati.
    Search {
        /// Testo cercato.
        query: String,
        /// Numero crescente della richiesta.
        generation: u64,
    },
    /// Indirizzo di un canale da aprire.
    ResolveStream {
        /// Canale.
        channel_id: i64,
        /// Numero della richiesta (per ignorare risposte di canali già lasciati).
        token: u64,
    },
    /// Logo da scaricare (o leggere dalla cache).
    FetchLogo(String),
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
}

impl From<StoreError> for SourceError {
    fn from(err: StoreError) -> Self {
        Self::Store(err.to_string())
    }
}

/// Esito della lettura di una playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReport {
    /// Canali letti.
    pub channels: usize,
    /// Categorie distinte.
    pub groups: usize,
    /// Righe saltate (doppioni, indirizzi non validi…).
    pub skipped: usize,
    /// La playlist indica una guida dei programmi.
    pub has_epg: bool,
}

/// Un logo pronto da mostrare (RGBA, 8 bit per canale).
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

/// Eventi verso l'interfaccia.
#[derive(Debug, Clone)]
pub enum Event {
    /// Impostazioni salvate (all'avvio).
    Settings(Vec<(String, String)>),
    /// Elenco delle sorgenti (dopo ogni modifica).
    Sources(Vec<SourceInfo>),
    /// Canali della sorgente in uso (`None`: nessuna sorgente).
    Channels {
        /// Sorgente.
        source_id: Option<i64>,
        /// Canali in ordine.
        channels: Arc<Vec<ChannelRow>>,
    },
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
        /// Canali trovati, nell'ordine della playlist.
        ids: Vec<i64>,
    },
    /// Indirizzo di un canale.
    Stream {
        /// Numero della richiesta.
        token: u64,
        /// Canale.
        channel_id: i64,
        /// Esito.
        result: Result<StreamInfo, String>,
    },
    /// Logo pronto (`None`: non disponibile).
    Logo {
        /// Indirizzo richiesto.
        url: String,
        /// Immagine.
        image: Option<LogoImage>,
    },
    /// Errore da mostrare.
    Error(String),
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
    http: reqwest::Client,
    on_event: EventSink,
    cache_dir: PathBuf,
    logos: Semaphore,
    logos_in_flight: Mutex<HashSet<String>>,
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
        std::fs::create_dir_all(config.cache_dir.join("loghi"))?;
        let store = Store::open(&config.database, config.cipher)?;
        let http = reqwest::Client::builder()
            .user_agent(config.user_agent)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
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
            http,
            on_event: Arc::new(on_event),
            cache_dir: config.cache_dir,
            logos: Semaphore::new(LOGO_PARALLELISM),
            logos_in_flight: Mutex::new(HashSet::new()),
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
            emit_sources_and_channels(&shared).await;
        }
        Command::VerifySource(source) => {
            shared.emit(Event::Working(true));
            let result = load_playlist(&shared, &source).await.map(|p| report(&p));
            shared.emit(Event::Working(false));
            shared.emit(Event::Verified(result));
        }
        Command::AddSource(source) => {
            shared.emit(Event::Working(true));
            let result = add_source(&shared, source).await;
            shared.emit(Event::Working(false));
            let (source_id, result) = match result {
                Ok((id, report)) => (Some(id), Ok(report)),
                Err(err) => (None, Err(err)),
            };
            shared.emit(Event::SourceSaved { source_id, result });
            emit_sources_and_channels(&shared).await;
        }
        Command::RefreshSource(id) => {
            shared.emit(Event::Working(true));
            let result = refresh_source(&shared, id).await;
            shared.emit(Event::Working(false));
            shared.emit(Event::SourceSaved {
                source_id: Some(id),
                result,
            });
            emit_sources_and_channels(&shared).await;
        }
        Command::ActivateSource(id) => {
            if let Err(err) = shared.with_store(move |s| s.set_active_source(id)).await {
                shared.emit(Event::Error(err.to_string()));
            }
            emit_sources_and_channels(&shared).await;
        }
        Command::DeleteSource(id) => {
            if let Err(err) = shared.with_store(move |s| s.delete_source(id)).await {
                shared.emit(Event::Error(err.to_string()));
            }
            emit_sources_and_channels(&shared).await;
        }
        Command::Search { query, generation } => {
            let result = shared
                .with_store(move |s| match s.active_source()? {
                    Some(source) => s.search(source, &query, SEARCH_LIMIT),
                    None => Ok(Vec::new()),
                })
                .await;
            match result {
                Ok(ids) => shared.emit(Event::SearchResults { generation, ids }),
                Err(err) => shared.emit(Event::Error(err.to_string())),
            }
        }
        Command::ResolveStream { channel_id, token } => {
            let result = shared
                .with_store(move |s| s.stream(channel_id))
                .await
                .map_err(|e| e.to_string());
            shared.emit(Event::Stream {
                token,
                channel_id,
                result,
            });
        }
        Command::FetchLogo(url) => fetch_logo(&shared, url).await,
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

async fn emit_sources_and_channels(shared: &Arc<Shared>) {
    let result = shared
        .with_store(|s| {
            let sources = s.sources()?;
            let active = s.active_source()?;
            let channels = match active {
                Some(id) => s.channels(id)?,
                None => Vec::new(),
            };
            Ok((sources, active, channels))
        })
        .await;
    match result {
        Ok((sources, source_id, channels)) => {
            shared.emit(Event::Sources(sources));
            shared.emit(Event::Channels {
                source_id,
                channels: Arc::new(channels),
            });
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
    }
}

async fn add_source(
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
    let playlist = load_playlist(shared, &source).await?;
    let report = report(&playlist);
    shared
        .with_store(move |s| s.replace_channels(id, &playlist.entries, &playlist.epg_urls, now()))
        .await?;
    Ok(report)
}

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

/// Nome del file di cache di un logo: hash dell'indirizzo (non si salvano URL su disco).
fn logo_cache_name(url: &str) -> String {
    Sha256::digest(url.as_bytes())
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn fetch_logo(shared: &Arc<Shared>, url: String) {
    {
        let mut in_flight = shared
            .logos_in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !in_flight.insert(url.clone()) {
            return;
        }
    }
    let image = load_logo(shared, &url).await;
    shared
        .logos_in_flight
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&url);
    shared.emit(Event::Logo { url, image });
}

async fn load_logo(shared: &Arc<Shared>, url: &str) -> Option<LogoImage> {
    let path = shared.cache_dir.join("loghi").join(logo_cache_name(url));
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(_) => {
            let _permit = shared.logos.acquire().await.ok()?;
            let bytes = download_logo(&shared.http, url).await?;
            if let Err(err) = tokio::fs::write(&path, &bytes).await {
                log::debug!("cache del logo non scritta: {err}");
            }
            bytes
        }
    };
    tokio::task::spawn_blocking(move || decode_logo(&bytes))
        .await
        .ok()
        .flatten()
}

async fn download_logo(http: &reqwest::Client, url: &str) -> Option<Vec<u8>> {
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
            .is_some_and(|len| len > MAX_LOGO_BYTES)
    {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    (bytes.len() as u64 <= MAX_LOGO_BYTES).then(|| bytes.to_vec())
}

fn decode_logo(bytes: &[u8]) -> Option<LogoImage> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width() > LOGO_MAX_SIDE || image.height() > LOGO_MAX_SIDE {
        image.thumbnail(LOGO_MAX_SIDE, LOGO_MAX_SIDE)
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
    use std::sync::mpsc as std_mpsc;

    fn start(dir: &std::path::Path) -> (Service, std_mpsc::Receiver<Event>) {
        let (tx, rx) = std_mpsc::channel();
        let tx = Mutex::new(tx);
        let service = Service::start(
            ServiceConfig {
                database: dir.join("archivio.db"),
                cache_dir: dir.join("cache"),
                cipher: Cipher::new(&[3; 32]),
                user_agent: "Sintonia-test".into(),
            },
            move |event| {
                let _ = tx.lock().unwrap().send(event);
            },
        )
        .unwrap();
        (service, rx)
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

    #[test]
    fn add_file_source_then_search_and_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lista.m3u");
        std::fs::write(
            &file,
            "#EXTM3U\n#EXTINF:-1 group-title=\"News\",Notizie Uno\nhttp://h/u/p/1.ts\n\
             #EXTINF:-1 group-title=\"Sport\",Sport Due\nhttp://h/u/p/2.ts\n",
        )
        .unwrap();
        let (service, rx) = start(dir.path());
        service.send(Command::AddSource(NewSource {
            name: "Prova".into(),
            kind: SourceKind::M3uFile,
            location: file.to_string_lossy().into_owned(),
            user_agent: None,
        }));
        let saved = wait(&rx, |e| match e {
            Event::SourceSaved { result, .. } => Some(result),
            _ => None,
        });
        assert_eq!(saved.unwrap().channels, 2);
        let channels = wait(&rx, |e| match e {
            Event::Channels { channels, .. } => Some(channels),
            _ => None,
        });
        assert_eq!(channels.len(), 2);

        service.send(Command::Search {
            query: "sport".into(),
            generation: 7,
        });
        let (generation, ids) = wait(&rx, |e| match e {
            Event::SearchResults { generation, ids } => Some((generation, ids)),
            _ => None,
        });
        assert_eq!((generation, ids), (7, vec![channels[1].id]));

        service.send(Command::ResolveStream {
            channel_id: channels[0].id,
            token: 1,
        });
        let stream = wait(&rx, |e| match e {
            Event::Stream { result, .. } => Some(result),
            _ => None,
        });
        assert_eq!(stream.unwrap().url, "http://h/u/p/1.ts");
    }

    #[test]
    fn missing_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (service, rx) = start(dir.path());
        service.send(Command::VerifySource(NewSource {
            name: "X".into(),
            kind: SourceKind::M3uFile,
            location: dir
                .path()
                .join("non-esiste.m3u")
                .to_string_lossy()
                .into_owned(),
            user_agent: None,
        }));
        let result = wait(&rx, |e| match e {
            Event::Verified(result) => Some(result),
            _ => None,
        });
        assert!(matches!(result, Err(SourceError::File(_))));
    }

    #[test]
    fn logo_names_do_not_contain_the_url() {
        let name = logo_cache_name("http://h/u/p/logo.png");
        assert_eq!(name.len(), 32);
        assert!(!name.contains("logo"));
    }

    #[test]
    fn logos_are_decoded_and_shrunk() {
        let mut png = Vec::new();
        image::RgbaImage::new(400, 200)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let logo = decode_logo(&png).unwrap();
        assert_eq!((logo.width, logo.height), (128, 64));
        assert_eq!(logo.rgba.len(), 128 * 64 * 4);
        assert!(decode_logo(b"non un'immagine").is_none());
    }
}
