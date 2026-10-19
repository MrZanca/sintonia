//! Archivio SQLite: sorgenti, canali, film, serie, preferiti, cronologia, guida dei programmi,
//! impostazioni, ricerca full-text (FTS5).
//!
//! Gli indirizzi (della playlist e di ogni canale M3U) e le credenziali Xtream (server e utente)
//! sono cifrati con [`Cipher`]; la password Xtream non sta qui ma nel portachiavi
//! ([`crate::secrets`]). In chiaro restano solo i dati che servono a mostrare e cercare.
//!
//! Ogni contenuto ha una **chiave stabile** che sopravvive agli aggiornamenti della sorgente:
//! per i canali M3U è un hash (con un sale dell'installazione) dell'indirizzo, per Xtream
//! l'identificativo del server. Preferiti e cronologia si appoggiano a questa chiave.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};

use crate::crypto::{Cipher, CryptoError};
use crate::epg::{Matcher, Programme, XmltvChannel};
use crate::m3u::Entry;
use crate::redact::redact_url;

const SCHEMA_VERSION: i32 = 3;

/// Errori dell'archivio.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Errore di SQLite.
    #[error("archivio: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Un valore cifrato non si decifra.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// La sorgente o il contenuto non esiste più.
    #[error("elemento non trovato")]
    NotFound,
    /// L'operazione è stata interrotta (chiusura dell'app o panic).
    #[error("operazione sull'archivio interrotta")]
    Interrupted,
}

/// Tipo di sorgente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Playlist M3U scaricata da un indirizzo.
    M3uUrl,
    /// Playlist M3U da un file sul PC.
    M3uFile,
    /// Account Xtream Codes.
    Xtream,
}

impl SourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::M3uUrl => "m3u-url",
            Self::M3uFile => "m3u-file",
            Self::Xtream => "xtream",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "m3u-url" => Some(Self::M3uUrl),
            "m3u-file" => Some(Self::M3uFile),
            "xtream" => Some(Self::Xtream),
            _ => None,
        }
    }
}

/// Una sorgente come la vede l'interfaccia (senza credenziali).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    /// Identificativo.
    pub id: i64,
    /// Nome scelto dall'utente.
    pub name: String,
    /// Tipo.
    pub kind: SourceKind,
    /// Indirizzo ridotto a `schema://host/…` o nome del file.
    pub display: String,
    /// User-agent per scaricare la playlist, se diverso da quello generale.
    pub user_agent: Option<String>,
    /// Sorgente in uso.
    pub active: bool,
    /// Canali all'ultimo aggiornamento.
    pub channel_count: u32,
    /// Film all'ultimo aggiornamento (Xtream).
    pub movie_count: u32,
    /// Serie all'ultimo aggiornamento (Xtream).
    pub series_count: u32,
    /// Ultimo aggiornamento (secondi Unix).
    pub refreshed_at: Option<i64>,
    /// Scadenza dell'account Xtream (secondi Unix), se nota.
    pub expires: Option<i64>,
    /// Ultimo aggiornamento della guida (secondi Unix).
    pub epg_refreshed_at: Option<i64>,
    /// Canali con la guida.
    pub epg_matched: u32,
    /// Programmi in archivio.
    pub epg_programmes: u32,
}

/// Dati di una sorgente (nuova, o letta per aggiornarla).
#[derive(Clone, PartialEq, Eq)]
pub struct NewSource {
    /// Nome.
    pub name: String,
    /// Tipo.
    pub kind: SourceKind,
    /// Indirizzo della playlist, percorso del file o server Xtream (sensibile).
    pub location: String,
    /// Utente Xtream.
    pub username: Option<String>,
    /// User-agent per la playlist.
    pub user_agent: Option<String>,
    /// Guida dei programmi scelta dall'utente (indirizzo o file), al posto di quella indicata
    /// dalla playlist o dal server (sensibile come la playlist).
    pub epg_url: Option<String>,
}

impl std::fmt::Debug for NewSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewSource")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("location", &redact_url(&self.location))
            .finish_non_exhaustive()
    }
}

/// Un canale da salvare (da una playlist M3U o da Xtream).
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ChannelInput {
    /// Chiave stabile.
    pub key: String,
    /// Numero mostrato.
    pub number: u32,
    /// Nome.
    pub name: String,
    /// Categoria.
    pub group: String,
    /// Identificativo per la guida.
    pub tvg_id: Option<String>,
    /// Nome per la guida.
    pub tvg_name: Option<String>,
    /// Logo.
    pub logo: Option<String>,
    /// Indirizzo (M3U); `None` per Xtream, dove si costruisce al momento.
    pub url: Option<String>,
    /// User-agent del canale.
    pub user_agent: Option<String>,
    /// Referrer del canale.
    pub referrer: Option<String>,
    /// Radio.
    pub radio: bool,
    /// Protetto secondo la playlist.
    pub drm: bool,
    /// Identificativo Xtream del flusso.
    pub xtream_id: Option<i64>,
}

impl std::fmt::Debug for ChannelInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelInput")
            .field("name", &self.name)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// Un canale nell'elenco (senza indirizzo).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelRow {
    /// Identificativo (cambia quando la sorgente si aggiorna).
    pub id: i64,
    /// Chiave stabile (non cambia con gli aggiornamenti).
    pub key: String,
    /// Numero mostrato.
    pub number: u32,
    /// Nome.
    pub name: String,
    /// Categoria (vuota se la playlist non la indica).
    pub group: String,
    /// Indirizzo del logo.
    pub logo: Option<String>,
    /// Identificativo per la guida.
    pub tvg_id: Option<String>,
    /// Canale radio.
    pub radio: bool,
    /// Protetto da DRM secondo la playlist.
    pub drm: bool,
}

/// Un film (Xtream).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MovieRow {
    /// Identificativo nell'archivio.
    pub id: i64,
    /// Identificativo Xtream.
    pub xtream_id: i64,
    /// Titolo.
    pub name: String,
    /// Categoria.
    pub group: String,
    /// Locandina.
    pub cover: Option<String>,
    /// Voto su 10.
    pub rating: Option<f32>,
    /// Anno.
    pub year: Option<String>,
    /// Aggiunto il.
    pub added: Option<i64>,
    /// Estensione del file.
    pub container: Option<String>,
}

impl MovieRow {
    /// Chiave stabile.
    pub fn key(&self) -> String {
        format!("m{}", self.xtream_id)
    }
}

/// Una serie (Xtream).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesRow {
    /// Identificativo nell'archivio.
    pub id: i64,
    /// Identificativo Xtream.
    pub xtream_id: i64,
    /// Titolo.
    pub name: String,
    /// Categoria.
    pub group: String,
    /// Locandina.
    pub cover: Option<String>,
    /// Voto su 10.
    pub rating: Option<f32>,
    /// Anno.
    pub year: Option<String>,
    /// Trama.
    pub plot: Option<String>,
    /// Genere.
    pub genre: Option<String>,
}

impl SeriesRow {
    /// Chiave stabile.
    pub fn key(&self) -> String {
        format!("s{}", self.xtream_id)
    }
}

/// Tipo di contenuto per preferiti e cronologia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// Canale live.
    Live,
    /// Film.
    Movie,
    /// Serie (preferiti) o episodio (cronologia).
    Series,
}

impl ItemKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Movie => "movie",
            Self::Series => "series",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "live" => Some(Self::Live),
            "movie" => Some(Self::Movie),
            "series" => Some(Self::Series),
            _ => None,
        }
    }
}

/// Una voce della cronologia («Riprendi»).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HistoryEntry {
    /// Tipo.
    pub kind: Option<ItemKind>,
    /// Chiave del contenuto (per una serie: la chiave della serie).
    pub key: String,
    /// Titolo mostrato.
    pub title: String,
    /// Riga sotto il titolo (categoria, «S2 · E4»…).
    pub subtitle: String,
    /// Immagine.
    pub cover: Option<String>,
    /// Secondi visti.
    pub position: f64,
    /// Durata in secondi (0 se live o sconosciuta).
    pub duration: f64,
    /// Ultima visione (secondi Unix).
    pub watched_at: i64,
    /// Per le serie: episodio da riprendere (identificativo, stagione, numero, estensione).
    pub episode: Option<EpisodeRef>,
}

/// Riferimento a un episodio di una serie Xtream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EpisodeRef {
    /// Identificativo del flusso.
    pub id: i64,
    /// Stagione.
    pub season: u32,
    /// Numero.
    pub number: u32,
    /// Estensione del file.
    pub container: String,
}

/// Quello che serve al player per aprire un contenuto.
#[derive(Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// Indirizzo in chiaro: non va mai nei log.
    pub url: String,
    /// User-agent richiesto dal canale.
    pub user_agent: Option<String>,
    /// Referrer richiesto dal canale.
    pub referrer: Option<String>,
}

impl std::fmt::Debug for StreamInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamInfo")
            .field("url", &redact_url(&self.url))
            .finish_non_exhaustive()
    }
}

/// Dove trovare il flusso di un canale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelStream {
    /// Indirizzo completo (M3U).
    Url(StreamInfo),
    /// Canale Xtream: l'indirizzo si costruisce con le credenziali della sorgente.
    Xtream {
        /// Sorgente.
        source_id: i64,
        /// Identificativo del flusso.
        xtream_id: i64,
    },
}

/// Un programma della guida, già abbinato a un canale della sorgente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuideProgramme {
    /// Chiave stabile del canale.
    pub channel_key: String,
    /// Inizio (secondi Unix).
    pub start: i64,
    /// Fine (secondi Unix).
    pub stop: i64,
    /// Titolo.
    pub title: String,
    /// Sottotitolo.
    pub subtitle: Option<String>,
    /// Categoria.
    pub category: Option<String>,
}

/// Dettagli di un programma.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProgrammeDetail {
    /// Descrizione.
    pub description: Option<String>,
    /// Episodio.
    pub episode: Option<String>,
}

/// Esito di un'importazione della guida.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EpgSummary {
    /// Programmi in archivio.
    pub programmes: u32,
    /// Canali nella guida.
    pub channels: u32,
    /// Canali della sorgente abbinati a un canale della guida con programmi.
    pub matched: u32,
}

/// L'archivio.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    cipher: Cipher,
    salt: String,
}

impl Store {
    /// Apre (o crea) l'archivio in un file.
    pub fn open(path: &Path, cipher: Cipher) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn, cipher)
    }

    /// Archivio in memoria (per i test).
    pub fn open_in_memory(cipher: Cipher) -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?, cipher)
    }

    fn init(conn: Connection, cipher: Cipher) -> Result<Self, StoreError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(SCHEMA_V1)?;
            conn.pragma_update(None, "user_version", 1)?;
        }
        let mut store = Self {
            conn,
            cipher,
            salt: String::new(),
        };
        store.salt = store.load_salt()?;
        if version < 2 {
            store.migrate_v2()?;
        }
        if version < 3 {
            let tx = store.conn.transaction()?;
            tx.execute_batch(SCHEMA_V3)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        // un'importazione della guida interrotta (chiusura, crash) lascia righe in attesa
        for table in ["epg_programmes", "epg_channels"] {
            store
                .conn
                .execute(&format!("DELETE FROM {table} WHERE pending = 1"), [])?;
        }
        Ok(store)
    }

    /// Sale dell'installazione per le chiavi stabili (creato al primo avvio).
    fn load_salt(&self) -> Result<String, StoreError> {
        if let Some(salt) = self.setting("salt")? {
            return Ok(salt);
        }
        let salt: String = Cipher::generate_key()?
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect();
        self.set_setting("salt", &salt)?;
        Ok(salt)
    }

    /// Chiave stabile di un canale M3U: hash dell'indirizzo con il sale (non rivela l'indirizzo).
    pub fn m3u_key(&self, url: &str) -> String {
        let digest = Sha256::digest(format!("{}\n{url}", self.salt).as_bytes());
        let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
        format!("u{hex}")
    }

    fn migrate_v2(&mut self) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(SCHEMA_V2)?;
        // chiavi stabili dei canali M3U già salvati
        let rows: Vec<(i64, Vec<u8>)> = {
            let mut stmt = tx.prepare("SELECT id, url FROM channels")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        {
            let mut update = tx.prepare("UPDATE channels SET stable_key = ?2 WHERE id = ?1")?;
            for (id, blob) in rows {
                let key = match self.cipher.decrypt(&blob) {
                    Ok(url) => {
                        let digest = Sha256::digest(format!("{}\n{url}", self.salt).as_bytes());
                        let hex: String =
                            digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
                        format!("u{hex}")
                    }
                    Err(_) => format!("x{id}"),
                };
                update.execute(params![id, key])?;
            }
        }
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
        Ok(())
    }

    // ───────────── sorgenti ─────────────

    /// Salva una sorgente nuova; se è la prima diventa quella in uso.
    pub fn add_source(&mut self, source: &NewSource) -> Result<i64, StoreError> {
        let display = match source.kind {
            SourceKind::M3uUrl => redact_url(&source.location).into_owned(),
            SourceKind::M3uFile => Path::new(&source.location).file_name().map_or_else(
                || source.location.clone(),
                |n| n.to_string_lossy().into_owned(),
            ),
            SourceKind::Xtream => redact_url(&source.location)
                .trim_end_matches("/…")
                .to_owned(),
        };
        let secret = match &source.username {
            Some(user) => format!("{}\n{user}", source.location),
            None => source.location.clone(),
        };
        let location = self.cipher.encrypt(&secret)?;
        let epg_custom = source
            .epg_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(|u| self.cipher.encrypt(u))
            .transpose()?;
        let tx = self.conn.transaction()?;
        let has_active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sources WHERE active = 1)",
            [],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO sources(name, kind, location, display, user_agent, active, epg_custom)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                source.name,
                source.kind.as_str(),
                location,
                display,
                source.user_agent,
                !has_active,
                epg_custom
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Tutte le sorgenti, in ordine di creazione.
    pub fn sources(&self) -> Result<Vec<SourceInfo>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, display, user_agent, active, channel_count, movie_count,
                    series_count, refreshed_at, expires, epg_refreshed_at, epg_matched,
                    epg_programmes
             FROM sources ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SourceInfo {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: SourceKind::parse(&r.get::<_, String>(2)?).unwrap_or(SourceKind::M3uUrl),
                display: r.get(3)?,
                user_agent: r.get(4)?,
                active: r.get(5)?,
                channel_count: r.get(6)?,
                movie_count: r.get(7)?,
                series_count: r.get(8)?,
                refreshed_at: r.get(9)?,
                expires: r.get(10)?,
                epg_refreshed_at: r.get(11)?,
                epg_matched: r.get(12)?,
                epg_programmes: r.get(13)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// La sorgente in uso.
    pub fn active_source(&self) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT id FROM sources WHERE active = 1", [], |r| r.get(0))
            .optional()?)
    }

    /// Dati della sorgente in chiaro (indirizzo, utente Xtream), con il tipo e l'user-agent.
    pub fn source_location(&self, id: i64) -> Result<NewSource, StoreError> {
        type Row = (String, String, Vec<u8>, Option<String>, Option<Vec<u8>>);
        let (name, kind, location, user_agent, epg_custom): Row = self
            .conn
            .query_row(
                "SELECT name, kind, location, user_agent, epg_custom FROM sources WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        let epg_url = epg_custom
            .map(|blob| self.cipher.decrypt(&blob))
            .transpose()?;
        let kind = SourceKind::parse(&kind).unwrap_or(SourceKind::M3uUrl);
        let secret = self.cipher.decrypt(&location)?;
        let (location, username) = match (kind, secret.split_once('\n')) {
            (SourceKind::Xtream, Some((server, user))) => {
                (server.to_owned(), Some(user.to_owned()))
            }
            _ => (secret, None),
        };
        Ok(NewSource {
            name,
            kind,
            location,
            username,
            user_agent,
            epg_url,
        })
    }

    /// Estensione dei canali live e scadenza dell'account Xtream.
    pub fn set_account_info(
        &self,
        id: i64,
        live_ext: &str,
        expires: Option<i64>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sources SET live_ext = ?2, expires = ?3 WHERE id = ?1",
            params![id, live_ext, expires],
        )?;
        Ok(())
    }

    /// Estensione dei canali live di una sorgente Xtream ("m3u8" se mai verificata).
    pub fn live_extension(&self, id: i64) -> Result<String, StoreError> {
        let ext: Option<String> = self
            .conn
            .query_row("SELECT live_ext FROM sources WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or(StoreError::NotFound)?;
        Ok(ext.unwrap_or_else(|| "m3u8".to_owned()))
    }

    /// Indirizzi della guida dichiarati dalla playlist della sorgente.
    pub fn epg_urls(&self, id: i64) -> Result<Vec<String>, StoreError> {
        let blob: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT epg_urls FROM sources WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or(StoreError::NotFound)?;
        match blob {
            Some(blob) => Ok(self
                .cipher
                .decrypt(&blob)?
                .lines()
                .map(str::to_owned)
                .collect()),
            None => Ok(Vec::new()),
        }
    }

    /// Rende `id` la sorgente in uso.
    pub fn set_active_source(&mut self, id: i64) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sources WHERE id = ?1)",
            [id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StoreError::NotFound);
        }
        tx.execute("UPDATE sources SET active = (id = ?1)", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Elimina una sorgente e tutto ciò che le appartiene. Se era in uso passa alla prima rimasta.
    pub fn delete_source(&mut self, id: i64) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM sources WHERE id = ?1", [id])?;
        tx.execute(
            "UPDATE sources SET active = 1
             WHERE id = (SELECT MIN(id) FROM sources)
               AND NOT EXISTS(SELECT 1 FROM sources WHERE active = 1)",
            [],
        )?;
        rebuild_fts(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Sostituisce i canali di una sorgente M3U con quelli appena letti.
    pub fn replace_channels(
        &mut self,
        source_id: i64,
        entries: &[Entry],
        epg_urls: &[String],
        now: i64,
    ) -> Result<u32, StoreError> {
        let inputs: Vec<ChannelInput> = entries
            .iter()
            .enumerate()
            .map(|(position, e)| ChannelInput {
                key: self.m3u_key(&e.url),
                number: e
                    .number
                    .unwrap_or_else(|| u32::try_from(position + 1).unwrap_or(u32::MAX)),
                name: e.name.clone(),
                group: e.group.clone().unwrap_or_default(),
                tvg_id: e.tvg_id.clone(),
                tvg_name: e.tvg_name.clone(),
                logo: e.logo.clone(),
                url: Some(e.url.clone()),
                user_agent: e.user_agent.clone(),
                referrer: e.referrer.clone(),
                radio: e.radio,
                drm: e.drm,
                xtream_id: None,
            })
            .collect();
        self.replace_catalog(source_id, &inputs, None, epg_urls, now)
    }

    /// Sostituisce tutto il catalogo di una sorgente: canali e, per Xtream, film e serie.
    pub fn replace_catalog(
        &mut self,
        source_id: i64,
        channels: &[ChannelInput],
        vod: Option<(&[MovieRow], &[SeriesRow])>,
        epg_urls: &[String],
        now: i64,
    ) -> Result<u32, StoreError> {
        let epg = if epg_urls.is_empty() {
            None
        } else {
            Some(self.cipher.encrypt(&epg_urls.join("\n"))?)
        };
        let encrypted: Vec<Vec<u8>> = channels
            .iter()
            .map(|c| match &c.url {
                Some(url) => self.cipher.encrypt(url),
                None => Ok(Vec::new()),
            })
            .collect::<Result<_, _>>()?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM channels WHERE source_id = ?1", [source_id])?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO channels(source_id, position, number, name, group_name, tvg_id,
                    tvg_name, logo, url, user_agent, referrer, radio, drm, stable_key, xtream_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            for (position, (c, url)) in channels.iter().zip(&encrypted).enumerate() {
                insert.execute(params![
                    source_id,
                    u32::try_from(position).unwrap_or(u32::MAX),
                    c.number,
                    c.name,
                    c.group,
                    c.tvg_id,
                    c.tvg_name,
                    c.logo,
                    url,
                    c.user_agent,
                    c.referrer,
                    c.radio,
                    c.drm,
                    c.key,
                    c.xtream_id,
                ])?;
            }
        }
        let (movie_count, series_count) = match vod {
            Some((movies, series)) => {
                insert_vod(&tx, source_id, movies, series)?;
                (movies.len(), series.len())
            }
            None => (0, 0),
        };
        let count = u32::try_from(channels.len()).unwrap_or(u32::MAX);
        let changed = tx.execute(
            "UPDATE sources SET channel_count = ?2, refreshed_at = ?3, epg_urls = ?4,
                movie_count = ?5, series_count = ?6
             WHERE id = ?1",
            params![
                source_id,
                count,
                now,
                epg,
                u32::try_from(movie_count).unwrap_or(u32::MAX),
                u32::try_from(series_count).unwrap_or(u32::MAX)
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        rebuild_fts(&tx)?;
        // canali nuovi o rinominati: si rifà l'abbinamento con la guida già in archivio
        rebuild_epg_map(&tx, source_id)?;
        tx.commit()?;
        Ok(count)
    }

    // ───────────── guida dei programmi ─────────────

    /// Prepara l'importazione di una guida: toglie i resti di un'importazione interrotta.
    /// I dati nuovi restano «in attesa» finché [`Self::epg_commit`] non li sostituisce ai vecchi:
    /// intanto la guida precedente resta visibile.
    pub fn epg_begin(&mut self, source_id: i64) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for table in ["epg_programmes", "epg_channels"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE source_id = ?1 AND pending = 1"),
                [source_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Aggiunge un blocco di canali e programmi all'importazione in corso.
    pub fn epg_insert(
        &mut self,
        source_id: i64,
        channels: &[XmltvChannel],
        programmes: &[Programme],
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO epg_channels(source_id, pending, xmltv_id, names)
                 VALUES (?1, 1, ?2, ?3)",
            )?;
            for c in channels {
                insert.execute(params![source_id, c.id, c.names.join("\n")])?;
            }
            let mut insert = tx.prepare_cached(
                "INSERT INTO epg_programmes(source_id, pending, xmltv_id, start, stop, title,
                    subtitle, description, category, episode)
                 VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for p in programmes {
                insert.execute(params![
                    source_id,
                    p.channel,
                    p.start,
                    p.stop,
                    p.title,
                    p.subtitle,
                    p.description,
                    p.category,
                    p.episode
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Sostituisce la guida della sorgente con quella appena importata e rifà l'abbinamento.
    pub fn epg_commit(&mut self, source_id: i64, now: i64) -> Result<EpgSummary, StoreError> {
        let tx = self.conn.transaction()?;
        for table in ["epg_programmes", "epg_channels"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE source_id = ?1 AND pending = 0"),
                [source_id],
            )?;
            tx.execute(
                &format!("UPDATE {table} SET pending = 0 WHERE source_id = ?1"),
                [source_id],
            )?;
        }
        let matched = rebuild_epg_map(&tx, source_id)?;
        let count = |table: &str| -> Result<u32, StoreError> {
            Ok(tx.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE source_id = ?1"),
                [source_id],
                |r| r.get(0),
            )?)
        };
        let programmes = count("epg_programmes")?;
        let channels = count("epg_channels")?;
        tx.execute(
            "UPDATE sources SET epg_refreshed_at = ?2, epg_programmes = ?3 WHERE id = ?1",
            params![source_id, now, programmes],
        )?;
        tx.commit()?;
        Ok(EpgSummary {
            programmes,
            channels,
            matched,
        })
    }

    /// Annulla un'importazione non riuscita (la guida precedente resta).
    pub fn epg_abort(&mut self, source_id: i64) -> Result<(), StoreError> {
        self.epg_begin(source_id)
    }

    /// Guida scelta dall'utente per una sorgente (`None`: quella della playlist o del server).
    /// La guida già importata si toglie: la prossima importazione userà la nuova.
    pub fn set_epg_custom(&mut self, source_id: i64, url: Option<&str>) -> Result<(), StoreError> {
        let blob = url
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(|u| self.cipher.encrypt(u))
            .transpose()?;
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sources SET epg_custom = ?2, epg_refreshed_at = NULL, epg_matched = 0,
                epg_programmes = 0
             WHERE id = ?1",
            params![source_id, blob],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        for table in ["epg_programmes", "epg_channels", "epg_map"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE source_id = ?1"),
                [source_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Ultimo aggiornamento della guida di una sorgente.
    pub fn epg_refreshed_at(&self, source_id: i64) -> Result<Option<i64>, StoreError> {
        self.conn
            .query_row(
                "SELECT epg_refreshed_at FROM sources WHERE id = ?1",
                [source_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// I programmi della sorgente tra `from` e `to` (secondi Unix), per canale e in ordine.
    pub fn guide(
        &self,
        source_id: i64,
        from: i64,
        to: i64,
    ) -> Result<Vec<GuideProgramme>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT m.channel_key, p.start, p.stop, p.title, p.subtitle, p.category
             FROM epg_map m
             JOIN epg_programmes p
               ON p.source_id = m.source_id AND p.pending = 0 AND p.xmltv_id = m.xmltv_id
              AND p.start < ?3 AND p.stop > ?2
             WHERE m.source_id = ?1
             ORDER BY m.channel_key, p.start",
        )?;
        let rows = stmt.query_map(params![source_id, from, to], |r| {
            Ok(GuideProgramme {
                channel_key: r.get(0)?,
                start: r.get(1)?,
                stop: r.get(2)?,
                title: r.get(3)?,
                subtitle: r.get(4)?,
                category: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Descrizione ed episodio di un programma.
    pub fn programme_detail(
        &self,
        source_id: i64,
        channel_key: &str,
        start: i64,
    ) -> Result<Option<ProgrammeDetail>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT p.description, p.episode
                 FROM epg_map m
                 JOIN epg_programmes p
                   ON p.source_id = m.source_id AND p.pending = 0 AND p.xmltv_id = m.xmltv_id
                 WHERE m.source_id = ?1 AND m.channel_key = ?2 AND p.start = ?3",
                params![source_id, channel_key, start],
                |r| {
                    Ok(ProgrammeDetail {
                        description: r.get(0)?,
                        episode: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    // ───────────── canali, film, serie ─────────────

    /// I canali di una sorgente nell'ordine della playlist.
    pub fn channels(&self, source_id: i64) -> Result<Vec<ChannelRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, stable_key, number, name, group_name, logo, tvg_id, radio, drm
             FROM channels WHERE source_id = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map([source_id], |r| {
            Ok(ChannelRow {
                id: r.get(0)?,
                key: r.get(1)?,
                number: r.get(2)?,
                name: r.get(3)?,
                group: r.get(4)?,
                logo: r.get(5)?,
                tvg_id: r.get(6)?,
                radio: r.get(7)?,
                drm: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// I film di una sorgente.
    pub fn movies(&self, source_id: i64) -> Result<Vec<MovieRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, xtream_id, name, group_name, cover, rating, year, added, container
             FROM movies WHERE source_id = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map([source_id], |r| {
            Ok(MovieRow {
                id: r.get(0)?,
                xtream_id: r.get(1)?,
                name: r.get(2)?,
                group: r.get(3)?,
                cover: r.get(4)?,
                rating: r.get(5)?,
                year: r.get(6)?,
                added: r.get(7)?,
                container: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Le serie di una sorgente.
    pub fn series(&self, source_id: i64) -> Result<Vec<SeriesRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, xtream_id, name, group_name, cover, rating, year, plot, genre
             FROM series WHERE source_id = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map([source_id], |r| {
            Ok(SeriesRow {
                id: r.get(0)?,
                xtream_id: r.get(1)?,
                name: r.get(2)?,
                group: r.get(3)?,
                cover: r.get(4)?,
                rating: r.get(5)?,
                year: r.get(6)?,
                plot: r.get(7)?,
                genre: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Dove trovare il flusso di un canale.
    pub fn channel_stream(&self, channel_id: i64) -> Result<ChannelStream, StoreError> {
        let (source_id, url, user_agent, referrer, xtream_id): (
            i64,
            Vec<u8>,
            Option<String>,
            Option<String>,
            Option<i64>,
        ) = self
            .conn
            .query_row(
                "SELECT source_id, url, user_agent, referrer, xtream_id FROM channels WHERE id = ?1",
                [channel_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        match xtream_id {
            Some(xtream_id) if url.is_empty() => Ok(ChannelStream::Xtream {
                source_id,
                xtream_id,
            }),
            _ => Ok(ChannelStream::Url(StreamInfo {
                url: self.cipher.decrypt(&url)?,
                user_agent,
                referrer,
            })),
        }
    }

    /// Indirizzo (decifrato) e intestazioni di un canale M3U.
    pub fn stream(&self, channel_id: i64) -> Result<StreamInfo, StoreError> {
        match self.channel_stream(channel_id)? {
            ChannelStream::Url(info) => Ok(info),
            ChannelStream::Xtream { .. } => Err(StoreError::NotFound),
        }
    }

    /// Sorgente, identificativo Xtream ed estensione di un film.
    pub fn movie_ref(&self, movie_id: i64) -> Result<(i64, MovieRow), StoreError> {
        self.conn
            .query_row(
                "SELECT source_id, id, xtream_id, name, group_name, cover, rating, year, added, container
                 FROM movies WHERE id = ?1",
                [movie_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        MovieRow {
                            id: r.get(1)?,
                            xtream_id: r.get(2)?,
                            name: r.get(3)?,
                            group: r.get(4)?,
                            cover: r.get(5)?,
                            rating: r.get(6)?,
                            year: r.get(7)?,
                            added: r.get(8)?,
                            container: r.get(9)?,
                        },
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// Sorgente e dati di una serie.
    pub fn series_ref(&self, series_row_id: i64) -> Result<(i64, SeriesRow), StoreError> {
        self.conn
            .query_row(
                "SELECT source_id, id, xtream_id, name, group_name, cover, rating, year, plot, genre
                 FROM series WHERE id = ?1",
                [series_row_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        SeriesRow {
                            id: r.get(1)?,
                            xtream_id: r.get(2)?,
                            name: r.get(3)?,
                            group: r.get(4)?,
                            cover: r.get(5)?,
                            rating: r.get(6)?,
                            year: r.get(7)?,
                            plot: r.get(8)?,
                            genre: r.get(9)?,
                        },
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// Ricerca istantanea su nome e categoria: ogni parola è un prefisso ("rai sp" trova
    /// "Rai Sport"). Accenti e maiuscole non contano.
    pub fn search(&self, source_id: i64, query: &str, limit: u32) -> Result<Vec<i64>, StoreError> {
        self.search_in("channels", source_id, query, limit)
    }

    /// Come [`Self::search`], sui film.
    pub fn search_movies(
        &self,
        source_id: i64,
        query: &str,
        limit: u32,
    ) -> Result<Vec<i64>, StoreError> {
        self.search_in("movies", source_id, query, limit)
    }

    /// Come [`Self::search`], sulle serie.
    pub fn search_series(
        &self,
        source_id: i64,
        query: &str,
        limit: u32,
    ) -> Result<Vec<i64>, StoreError> {
        self.search_in("series", source_id, query, limit)
    }

    fn search_in(
        &self,
        table: &str,
        source_id: i64,
        query: &str,
        limit: u32,
    ) -> Result<Vec<i64>, StoreError> {
        let Some(expr) = fts_query(query) else {
            return Ok(Vec::new());
        };
        // `table` è sempre una costante del codice, mai un dato dell'utente
        let sql = format!(
            "SELECT c.id FROM {table}_fts f JOIN {table} c ON c.id = f.rowid
             WHERE {table}_fts MATCH ?1 AND c.source_id = ?2
             ORDER BY c.position LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let ids = stmt.query_map(params![expr, source_id, limit], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    // ───────────── preferiti e cronologia ─────────────

    /// Preferiti della sorgente: (tipo, chiave), i più recenti prima.
    pub fn favorites(&self, source_id: i64) -> Result<Vec<(ItemKind, String)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, key FROM favorites WHERE source_id = ?1 ORDER BY added_at DESC, rowid DESC",
        )?;
        let rows = stmt.query_map([source_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (kind, key) = row?;
            if let Some(kind) = ItemKind::parse(&kind) {
                out.push((kind, key));
            }
        }
        Ok(out)
    }

    /// Aggiunge o toglie un preferito.
    pub fn set_favorite(
        &self,
        source_id: i64,
        kind: ItemKind,
        key: &str,
        favorite: bool,
        now: i64,
    ) -> Result<(), StoreError> {
        if favorite {
            self.conn.execute(
                "INSERT OR IGNORE INTO favorites(source_id, kind, key, added_at) VALUES (?1, ?2, ?3, ?4)",
                params![source_id, kind.as_str(), key, now],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM favorites WHERE source_id = ?1 AND kind = ?2 AND key = ?3",
                params![source_id, kind.as_str(), key],
            )?;
        }
        Ok(())
    }

    /// Salva (o aggiorna) una voce della cronologia.
    pub fn save_history(&self, source_id: i64, entry: &HistoryEntry) -> Result<(), StoreError> {
        let kind = entry.kind.unwrap_or(ItemKind::Live);
        let episode = entry
            .episode
            .as_ref()
            .map(|e| format!("{}|{}|{}|{}", e.id, e.season, e.number, e.container));
        self.conn.execute(
            "INSERT INTO history(source_id, kind, key, title, subtitle, cover, position, duration,
                watched_at, episode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(source_id, kind, key) DO UPDATE SET
                title = excluded.title, subtitle = excluded.subtitle, cover = excluded.cover,
                position = excluded.position, duration = excluded.duration,
                watched_at = excluded.watched_at, episode = excluded.episode",
            params![
                source_id,
                kind.as_str(),
                entry.key,
                entry.title,
                entry.subtitle,
                entry.cover,
                entry.position,
                entry.duration,
                entry.watched_at,
                episode
            ],
        )?;
        // la cronologia resta corta: le 200 voci più recenti per sorgente
        self.conn.execute(
            "DELETE FROM history WHERE source_id = ?1 AND rowid NOT IN (
                SELECT rowid FROM history WHERE source_id = ?1 ORDER BY watched_at DESC LIMIT 200)",
            [source_id],
        )?;
        Ok(())
    }

    /// Toglie una voce dalla cronologia (per esempio un film finito).
    pub fn remove_history(
        &self,
        source_id: i64,
        kind: ItemKind,
        key: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM history WHERE source_id = ?1 AND kind = ?2 AND key = ?3",
            params![source_id, kind.as_str(), key],
        )?;
        Ok(())
    }

    /// Cronologia della sorgente, le più recenti prima.
    pub fn history(&self, source_id: i64, limit: u32) -> Result<Vec<HistoryEntry>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, key, title, subtitle, cover, position, duration, watched_at, episode
             FROM history WHERE source_id = ?1 ORDER BY watched_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![source_id, limit], |r| {
            let episode: Option<String> = r.get(8)?;
            Ok(HistoryEntry {
                kind: ItemKind::parse(&r.get::<_, String>(0)?),
                key: r.get(1)?,
                title: r.get(2)?,
                subtitle: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                cover: r.get(4)?,
                position: r.get(5)?,
                duration: r.get(6)?,
                watched_at: r.get(7)?,
                episode: episode.as_deref().and_then(parse_episode_ref),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // ───────────── impostazioni ─────────────

    /// Tutte le impostazioni salvate (tranne quelle interne).
    pub fn settings(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM settings WHERE key <> 'salt' ORDER BY key")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Una impostazione.
    pub fn setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Salva una impostazione.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }
}

const SCHEMA_V1: &str = "BEGIN;
    CREATE TABLE sources(
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        location BLOB NOT NULL,
        display TEXT NOT NULL,
        user_agent TEXT,
        epg_urls BLOB,
        active INTEGER NOT NULL DEFAULT 0,
        channel_count INTEGER NOT NULL DEFAULT 0,
        refreshed_at INTEGER
    );
    CREATE TABLE channels(
        id INTEGER PRIMARY KEY,
        source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        number INTEGER NOT NULL,
        name TEXT NOT NULL,
        group_name TEXT NOT NULL,
        tvg_id TEXT,
        tvg_name TEXT,
        logo TEXT,
        url BLOB NOT NULL,
        user_agent TEXT,
        referrer TEXT,
        radio INTEGER NOT NULL DEFAULT 0,
        drm INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX channels_by_source ON channels(source_id, position);
    CREATE VIRTUAL TABLE channels_fts USING fts5(
        name, group_name,
        content='channels', content_rowid='id',
        tokenize='unicode61 remove_diacritics 2'
    );
    CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    COMMIT;";

/// Versione 2: Xtream (film, serie), chiavi stabili, preferiti, cronologia.
const SCHEMA_V2: &str = "
    ALTER TABLE sources ADD COLUMN movie_count INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE sources ADD COLUMN series_count INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE sources ADD COLUMN live_ext TEXT;
    ALTER TABLE sources ADD COLUMN expires INTEGER;
    ALTER TABLE channels ADD COLUMN stable_key TEXT NOT NULL DEFAULT '';
    ALTER TABLE channels ADD COLUMN xtream_id INTEGER;
    CREATE TABLE movies(
        id INTEGER PRIMARY KEY,
        source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        xtream_id INTEGER NOT NULL,
        name TEXT NOT NULL,
        group_name TEXT NOT NULL,
        cover TEXT,
        rating REAL,
        year TEXT,
        added INTEGER,
        container TEXT
    );
    CREATE INDEX movies_by_source ON movies(source_id, position);
    CREATE VIRTUAL TABLE movies_fts USING fts5(
        name, group_name, content='movies', content_rowid='id',
        tokenize='unicode61 remove_diacritics 2'
    );
    CREATE TABLE series(
        id INTEGER PRIMARY KEY,
        source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        xtream_id INTEGER NOT NULL,
        name TEXT NOT NULL,
        group_name TEXT NOT NULL,
        cover TEXT,
        rating REAL,
        year TEXT,
        plot TEXT,
        genre TEXT
    );
    CREATE INDEX series_by_source ON series(source_id, position);
    CREATE VIRTUAL TABLE series_fts USING fts5(
        name, group_name, content='series', content_rowid='id',
        tokenize='unicode61 remove_diacritics 2'
    );
    CREATE TABLE favorites(
        source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        key TEXT NOT NULL,
        added_at INTEGER NOT NULL,
        PRIMARY KEY(source_id, kind, key)
    );
    CREATE TABLE history(
        source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        key TEXT NOT NULL,
        title TEXT NOT NULL,
        subtitle TEXT,
        cover TEXT,
        position REAL NOT NULL DEFAULT 0,
        duration REAL NOT NULL DEFAULT 0,
        watched_at INTEGER NOT NULL,
        episode TEXT,
        PRIMARY KEY(source_id, kind, key)
    );
";

/// Versione 3: guida dei programmi. Le righe con `pending = 1` sono un'importazione in corso.
const SCHEMA_V3: &str = "
    ALTER TABLE sources ADD COLUMN epg_custom BLOB;
    ALTER TABLE sources ADD COLUMN epg_refreshed_at INTEGER;
    ALTER TABLE sources ADD COLUMN epg_matched INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE sources ADD COLUMN epg_programmes INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE epg_channels(
        source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
        pending INTEGER NOT NULL DEFAULT 0,
        xmltv_id TEXT NOT NULL,
        names TEXT NOT NULL
    );
    CREATE INDEX epg_channels_by_source ON epg_channels(source_id, pending);
    CREATE TABLE epg_programmes(
        source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
        pending INTEGER NOT NULL DEFAULT 0,
        xmltv_id TEXT NOT NULL,
        start INTEGER NOT NULL,
        stop INTEGER NOT NULL,
        title TEXT NOT NULL,
        subtitle TEXT,
        description TEXT,
        category TEXT,
        episode TEXT
    );
    CREATE INDEX epg_programmes_by_channel
        ON epg_programmes(source_id, pending, xmltv_id, start);
    CREATE TABLE epg_map(
        source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
        channel_key TEXT NOT NULL,
        xmltv_id TEXT NOT NULL,
        PRIMARY KEY(source_id, channel_key)
    );
";

fn insert_vod(
    tx: &Transaction<'_>,
    source_id: i64,
    movies: &[MovieRow],
    series: &[SeriesRow],
) -> Result<(), StoreError> {
    tx.execute("DELETE FROM movies WHERE source_id = ?1", [source_id])?;
    tx.execute("DELETE FROM series WHERE source_id = ?1", [source_id])?;
    let mut insert = tx.prepare(
        "INSERT INTO movies(source_id, position, xtream_id, name, group_name, cover, rating, year,
            added, container)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for (position, m) in movies.iter().enumerate() {
        insert.execute(params![
            source_id,
            u32::try_from(position).unwrap_or(u32::MAX),
            m.xtream_id,
            m.name,
            m.group,
            m.cover,
            m.rating,
            m.year,
            m.added,
            m.container
        ])?;
    }
    let mut insert = tx.prepare(
        "INSERT INTO series(source_id, position, xtream_id, name, group_name, cover, rating, year,
            plot, genre)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for (position, s) in series.iter().enumerate() {
        insert.execute(params![
            source_id,
            u32::try_from(position).unwrap_or(u32::MAX),
            s.xtream_id,
            s.name,
            s.group,
            s.cover,
            s.rating,
            s.year,
            s.plot,
            s.genre
        ])?;
    }
    Ok(())
}

fn rebuild_fts(tx: &Transaction<'_>) -> Result<(), StoreError> {
    for table in ["channels_fts", "movies_fts", "series_fts"] {
        tx.execute(
            &format!("INSERT INTO {table}({table}) VALUES ('rebuild')"),
            [],
        )?;
    }
    Ok(())
}

/// Abbina i canali della sorgente ai canali della guida (vedi [`Matcher`]) e salva `epg_map`.
/// A parità di nome vincono i canali della guida che hanno programmi.
fn rebuild_epg_map(tx: &Transaction<'_>, source_id: i64) -> Result<u32, StoreError> {
    tx.execute("DELETE FROM epg_map WHERE source_id = ?1", [source_id])?;
    let mut guide: Vec<(String, Vec<String>, bool)> = {
        let mut stmt = tx.prepare(
            "SELECT c.xmltv_id, c.names,
                    EXISTS(SELECT 1 FROM epg_programmes p
                           WHERE p.source_id = c.source_id AND p.pending = 0
                             AND p.xmltv_id = c.xmltv_id)
             FROM epg_channels c WHERE c.source_id = ?1 AND c.pending = 0",
        )?;
        let rows = stmt.query_map([source_id], |r| {
            let names: String = r.get(1)?;
            Ok((
                r.get::<_, String>(0)?,
                names.lines().map(str::to_owned).collect(),
                r.get::<_, bool>(2)?,
            ))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    guide.sort_by_key(|(_, _, has_programmes)| !has_programmes);
    let with_programmes: std::collections::HashSet<&str> = guide
        .iter()
        .filter(|(_, _, has)| *has)
        .map(|(id, _, _)| id.as_str())
        .collect();
    let matcher = Matcher::new(
        guide
            .iter()
            .map(|(id, names, _)| (id.as_str(), names.as_slice())),
    );
    let channels: Vec<(String, String, Option<String>)> = {
        let mut stmt =
            tx.prepare("SELECT stable_key, name, tvg_id FROM channels WHERE source_id = ?1")?;
        let rows = stmt.query_map([source_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut matched = 0u32;
    if !guide.is_empty() {
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO epg_map(source_id, channel_key, xmltv_id) VALUES (?1, ?2, ?3)",
        )?;
        for (key, name, tvg_id) in &channels {
            if let Some(id) = matcher.find(tvg_id.as_deref(), name) {
                insert.execute(params![source_id, key, id])?;
                if with_programmes.contains(id) {
                    matched += 1;
                }
            }
        }
    }
    tx.execute(
        "UPDATE sources SET epg_matched = ?2 WHERE id = ?1",
        params![source_id, matched],
    )?;
    Ok(matched)
}

fn parse_episode_ref(text: &str) -> Option<EpisodeRef> {
    let mut parts = text.splitn(4, '|');
    Some(EpisodeRef {
        id: parts.next()?.parse().ok()?,
        season: parts.next()?.parse().ok()?,
        number: parts.next()?.parse().ok()?,
        container: parts.next()?.to_owned(),
    })
}

/// Trasforma il testo cercato in un'espressione FTS5 sicura: parole come prefissi, in AND.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m3u::parse_str;

    fn store() -> Store {
        Store::open_in_memory(Cipher::new(&[9; 32])).unwrap()
    }

    fn source(store: &mut Store, name: &str) -> i64 {
        store
            .add_source(&NewSource {
                name: name.into(),
                kind: SourceKind::M3uUrl,
                location: "http://prov.example/get.php?username=mario&password=segreta".into(),
                username: None,
                user_agent: None,
                epg_url: None,
            })
            .unwrap()
    }

    const LIST: &str = "#EXTM3U url-tvg=\"http://epg/u/p.xml\"\n\
        #EXTINF:-1 tvg-chno=\"7\" group-title=\"Sport\",Rai Sport HD\nhttp://h/live/u/p/1.ts\n\
        #EXTINF:-1 group-title=\"Città\",Tèle Città\nhttp://h/live/u/p/2.ts\n\
        #EXTINF:-1,Senza gruppo\n#EXTVLCOPT:http-user-agent=UA\nhttp://h/live/u/p/3.ts\n";

    #[test]
    fn first_source_becomes_active_and_url_is_hidden() {
        let mut store = store();
        let a = source(&mut store, "Casa");
        let b = source(&mut store, "Ufficio");
        let sources = store.sources().unwrap();
        assert!(sources[0].active && !sources[1].active);
        assert_eq!(sources[0].display, "http://prov.example/…");
        assert_eq!(store.active_source().unwrap(), Some(a));
        let raw: Vec<u8> = store
            .conn
            .query_row("SELECT location FROM sources WHERE id = ?1", [b], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("segreta"));
        assert!(store
            .source_location(b)
            .unwrap()
            .location
            .contains("segreta"));
    }

    #[test]
    fn xtream_source_keeps_server_and_user_encrypted() {
        let mut store = store();
        let id = store
            .add_source(&NewSource {
                name: "Casa".into(),
                kind: SourceKind::Xtream,
                location: "http://tv.example:8080".into(),
                username: Some("mario".into()),
                user_agent: None,
                epg_url: None,
            })
            .unwrap();
        let info = &store.sources().unwrap()[0];
        assert_eq!(info.display, "http://tv.example:8080");
        let back = store.source_location(id).unwrap();
        assert_eq!(back.location, "http://tv.example:8080");
        assert_eq!(back.username.as_deref(), Some("mario"));
        store.set_account_info(id, "ts", Some(42)).unwrap();
        assert_eq!(store.live_extension(id).unwrap(), "ts");
        assert_eq!(store.sources().unwrap()[0].expires, Some(42));
    }

    #[test]
    fn channels_round_trip_with_encrypted_urls_and_stable_keys() {
        let mut store = store();
        let id = source(&mut store, "Casa");
        let list = parse_str(LIST);
        assert_eq!(
            store
                .replace_channels(id, &list.entries, &list.epg_urls, 100)
                .unwrap(),
            3
        );
        let rows = store.channels(id).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].number, rows[1].number, rows[2].number), (7, 2, 3));
        assert_eq!(rows[2].group, "");
        assert!(rows[0].key.starts_with('u') && rows[0].key != rows[1].key);
        assert!(!rows[0].key.contains("live"));
        let stream = store.stream(rows[2].id).unwrap();
        assert_eq!(stream.url, "http://h/live/u/p/3.ts");
        assert_eq!(stream.user_agent.as_deref(), Some("UA"));
        assert!(!format!("{stream:?}").contains("/u/p/"));
        assert_eq!(store.epg_urls(id).unwrap(), ["http://epg/u/p.xml"]);
        let leaked: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM channels WHERE CAST(url AS TEXT) LIKE '%live%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);
        assert_eq!(store.sources().unwrap()[0].channel_count, 3);

        // dopo un aggiornamento gli id possono cambiare (o essere riusati per altri canali):
        // l'identità vera è la chiave, che resta la stessa
        store
            .replace_channels(id, &list.entries[1..], &[], 200)
            .unwrap();
        let again = store.channels(id).unwrap();
        assert_eq!(again[0].key, rows[1].key);
    }

    #[test]
    fn xtream_catalog_and_streams() {
        let mut store = store();
        let id = source(&mut store, "X");
        let live = [ChannelInput {
            key: "l5".into(),
            number: 1,
            name: "Uno".into(),
            group: "News".into(),
            xtream_id: Some(5),
            ..ChannelInput::default()
        }];
        let movies = [MovieRow {
            xtream_id: 77,
            name: "Film à".into(),
            group: "Azione".into(),
            container: Some("mkv".into()),
            ..MovieRow::default()
        }];
        let series = [SeriesRow {
            xtream_id: 9,
            name: "Serie".into(),
            group: "Drama".into(),
            ..SeriesRow::default()
        }];
        store
            .replace_catalog(id, &live, Some((&movies, &series)), &[], 1)
            .unwrap();
        let channel = &store.channels(id).unwrap()[0];
        assert_eq!(
            store.channel_stream(channel.id).unwrap(),
            ChannelStream::Xtream {
                source_id: id,
                xtream_id: 5
            }
        );
        let movie = &store.movies(id).unwrap()[0];
        assert_eq!(movie.key(), "m77");
        assert_eq!(
            store.movie_ref(movie.id).unwrap().1.container.as_deref(),
            Some("mkv")
        );
        assert_eq!(store.search_movies(id, "film a", 10).unwrap(), [movie.id]);
        let serie = &store.series(id).unwrap()[0];
        assert_eq!(store.search_series(id, "drama", 10).unwrap(), [serie.id]);
        let info = &store.sources().unwrap()[0];
        assert_eq!((info.movie_count, info.series_count), (1, 1));
    }

    #[test]
    fn favorites_and_history() {
        let mut store = store();
        let id = source(&mut store, "A");
        store
            .set_favorite(id, ItemKind::Live, "u1", true, 1)
            .unwrap();
        store
            .set_favorite(id, ItemKind::Movie, "m2", true, 2)
            .unwrap();
        store
            .set_favorite(id, ItemKind::Live, "u1", true, 3)
            .unwrap();
        assert_eq!(
            store.favorites(id).unwrap(),
            [
                (ItemKind::Movie, "m2".to_owned()),
                (ItemKind::Live, "u1".to_owned())
            ]
        );
        store
            .set_favorite(id, ItemKind::Live, "u1", false, 4)
            .unwrap();
        assert_eq!(store.favorites(id).unwrap().len(), 1);

        let entry = HistoryEntry {
            kind: Some(ItemKind::Series),
            key: "s9".into(),
            title: "Serie".into(),
            subtitle: "S1 · E2".into(),
            position: 120.5,
            duration: 1500.0,
            watched_at: 10,
            episode: Some(EpisodeRef {
                id: 33,
                season: 1,
                number: 2,
                container: "mkv".into(),
            }),
            ..HistoryEntry::default()
        };
        store.save_history(id, &entry).unwrap();
        store
            .save_history(
                id,
                &HistoryEntry {
                    position: 300.0,
                    watched_at: 11,
                    ..entry.clone()
                },
            )
            .unwrap();
        let history = store.history(id, 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].position, 300.0);
        assert_eq!(history[0].episode, entry.episode);
        store.remove_history(id, ItemKind::Series, "s9").unwrap();
        assert!(store.history(id, 10).unwrap().is_empty());
    }

    #[test]
    fn search_is_prefix_accent_insensitive_and_per_source() {
        let mut store = store();
        let a = source(&mut store, "A");
        let b = source(&mut store, "B");
        let list = parse_str(LIST);
        store.replace_channels(a, &list.entries, &[], 1).unwrap();
        store.replace_channels(b, &list.entries, &[], 1).unwrap();
        let rows = store.channels(a).unwrap();
        assert_eq!(store.search(a, "rai sp", 50).unwrap(), [rows[0].id]);
        assert_eq!(store.search(a, "citta", 50).unwrap(), [rows[1].id]);
        assert_eq!(store.search(a, "SPORT", 50).unwrap(), [rows[0].id]);
        assert!(store.search(a, "\"*)(", 50).unwrap().is_empty());
        assert_eq!(store.search(b, "rai", 50).unwrap().len(), 1);
    }

    #[test]
    fn refresh_replaces_and_delete_cascades() {
        let mut store = store();
        let a = source(&mut store, "A");
        let b = source(&mut store, "B");
        let list = parse_str(LIST);
        store.replace_channels(a, &list.entries, &[], 1).unwrap();
        store
            .set_favorite(a, ItemKind::Live, "u1", true, 1)
            .unwrap();
        store
            .replace_channels(a, &list.entries[..1], &[], 2)
            .unwrap();
        assert_eq!(store.channels(a).unwrap().len(), 1);
        store.delete_source(a).unwrap();
        assert!(store.channels(a).unwrap().is_empty());
        assert!(store.favorites(a).unwrap().is_empty());
        assert_eq!(store.active_source().unwrap(), Some(b));
        assert!(store.search(b, "rai", 5).unwrap().is_empty());
        assert!(matches!(
            store.set_active_source(a),
            Err(StoreError::NotFound)
        ));
        assert_eq!(
            store.active_source().unwrap(),
            Some(b),
            "sorgente inesistente: niente cambia"
        );
    }

    #[test]
    fn settings_upsert_and_salt_is_hidden() {
        let store = store();
        store.set_setting("lingua", "it").unwrap();
        store.set_setting("lingua", "en").unwrap();
        assert_eq!(store.setting("lingua").unwrap().as_deref(), Some("en"));
        assert_eq!(store.settings().unwrap().len(), 1);
        assert!(store.setting("salt").unwrap().is_some());
    }

    #[test]
    fn version_1_archives_are_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.db");
        let cipher = Cipher::new(&[4; 32]);
        {
            // un archivio come lo scriveva M1
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO sources(id, name, kind, location, display, active, channel_count)
                 VALUES (1, 'Vecchia', 'm3u-file', ?1, 'lista.m3u', 1, 1)",
                [cipher.encrypt("C:/lista.m3u").unwrap()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO channels(source_id, position, number, name, group_name, url)
                 VALUES (1, 0, 1, 'Uno', 'News', ?1)",
                [cipher.encrypt("http://h/1.ts").unwrap()],
            )
            .unwrap();
        }
        let store = Store::open(&path, cipher).unwrap();
        let rows = store.channels(1).unwrap();
        assert_eq!(rows[0].key, store.m3u_key("http://h/1.ts"));
        assert_eq!(store.sources().unwrap()[0].name, "Vecchia");
        assert!(store.movies(1).unwrap().is_empty());
        let version: i32 = store
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    fn programme(channel: &str, start: i64, title: &str) -> Programme {
        Programme {
            channel: channel.into(),
            start,
            stop: start + 1800,
            title: title.into(),
            ..Programme::default()
        }
    }

    fn guide_channel(id: &str, name: &str) -> XmltvChannel {
        XmltvChannel {
            id: id.into(),
            names: vec![name.into()],
        }
    }

    #[test]
    fn guide_import_replaces_matches_and_queries() {
        let mut store = store();
        let id = source(&mut store, "Casa");
        // Rai Sport HD (nome), Tèle Città (nessuna guida), Senza gruppo (tvg-id assente)
        store
            .replace_channels(id, &parse_str(LIST).entries, &[], 1)
            .unwrap();
        let keys: Vec<String> = store
            .channels(id)
            .unwrap()
            .into_iter()
            .map(|c| c.key)
            .collect();

        store.epg_begin(id).unwrap();
        store
            .epg_insert(
                id,
                &[
                    guide_channel("RaiSport.it", "Rai Sport"),
                    guide_channel("Vuoto.it", "Senza gruppo"),
                ],
                &[programme("RaiSport.it", 1000, "Calcio")],
            )
            .unwrap();
        // finché non è confermata, la guida nuova non si vede
        assert!(store.guide(id, 0, 10_000).unwrap().is_empty());
        let summary = store.epg_commit(id, 50).unwrap();
        assert_eq!(
            summary,
            EpgSummary {
                programmes: 1,
                channels: 2,
                matched: 1
            },
            "«Senza gruppo» è abbinato ma senza programmi: non conta"
        );
        let guide = store.guide(id, 0, 10_000).unwrap();
        assert_eq!(guide.len(), 1);
        assert_eq!(guide[0].channel_key, keys[0]);
        assert_eq!(guide[0].title, "Calcio");
        assert!(
            store.guide(id, 2800, 9_000).unwrap().is_empty(),
            "fuori finestra"
        );
        let info = &store.sources().unwrap()[0];
        assert_eq!(
            (info.epg_refreshed_at, info.epg_matched, info.epg_programmes),
            (Some(50), 1, 1)
        );

        // seconda importazione: sostituisce, e un'importazione annullata non tocca niente
        store.epg_begin(id).unwrap();
        store
            .epg_insert(
                id,
                &[guide_channel("RaiSport.it", "Rai Sport")],
                &[
                    programme("RaiSport.it", 2000, "Tennis"),
                    programme("RaiSport.it", 3800, "Golf"),
                ],
            )
            .unwrap();
        store.epg_commit(id, 60).unwrap();
        store.epg_begin(id).unwrap();
        store
            .epg_insert(id, &[], &[programme("RaiSport.it", 9000, "Mai")])
            .unwrap();
        store.epg_abort(id).unwrap();
        let titles: Vec<String> = store
            .guide(id, 0, 100_000)
            .unwrap()
            .into_iter()
            .map(|p| p.title)
            .collect();
        assert_eq!(titles, ["Tennis", "Golf"]);

        // la playlist si aggiorna: l'abbinamento si rifà con la guida già in archivio
        store
            .replace_channels(id, &parse_str(LIST).entries[1..], &[], 2)
            .unwrap();
        assert!(store.guide(id, 0, 100_000).unwrap().is_empty());
        assert_eq!(store.sources().unwrap()[0].epg_matched, 0);
        // guida scelta dall'utente: la vecchia si toglie, l'indirizzo resta cifrato
        store
            .set_epg_custom(id, Some("http://epg.example/guida.xml.gz"))
            .unwrap();
        assert_eq!(
            store.source_location(id).unwrap().epg_url.as_deref(),
            Some("http://epg.example/guida.xml.gz")
        );
        assert_eq!(store.sources().unwrap()[0].epg_refreshed_at, None);
        store.set_epg_custom(id, None).unwrap();
        assert_eq!(store.source_location(id).unwrap().epg_url, None);
        store.delete_source(id).unwrap();
        let left: u32 = store
            .conn
            .query_row("SELECT COUNT(*) FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "la guida se ne va con la sorgente");
    }

    #[test]
    fn large_guide_is_imported_and_read_quickly() {
        let mut store = store();
        let id = source(&mut store, "Grande");
        let entries: Vec<Entry> = (0..2_000)
            .map(|i| Entry {
                name: format!("Canale {i}"),
                tvg_id: Some(format!("c{i}.it")),
                url: format!("http://h/live/u/p/{i}.ts"),
                ..Entry::default()
            })
            .collect();
        store.replace_channels(id, &entries, &[], 1).unwrap();
        let channels: Vec<XmltvChannel> = (0..2_000)
            .map(|i| guide_channel(&format!("c{i}.it"), &format!("Canale {i}")))
            .collect();
        let start = std::time::Instant::now();
        store.epg_begin(id).unwrap();
        // 2000 canali × 100 programmi, a blocchi come fa il servizio
        let mut batch = Vec::new();
        for i in 0..2_000 {
            for n in 0..100 {
                batch.push(programme(&format!("c{i}.it"), n * 1800, "Programma"));
            }
            if batch.len() >= 5_000 {
                store.epg_insert(id, &[], &batch).unwrap();
                batch.clear();
            }
        }
        store.epg_insert(id, &channels, &batch).unwrap();
        let summary = store.epg_commit(id, 1).unwrap();
        let imported = start.elapsed();
        assert_eq!(summary.programmes, 200_000);
        assert_eq!(summary.matched, 2_000);
        let start = std::time::Instant::now();
        let day = store.guide(id, 0, 86_400).unwrap();
        let read = start.elapsed();
        assert_eq!(day.len(), 2_000 * 48);
        // limiti larghi: valgono anche per le build di debug
        assert!(
            imported < std::time::Duration::from_secs(60),
            "importazione: {imported:?}"
        );
        assert!(
            read < std::time::Duration::from_secs(5),
            "lettura: {read:?}"
        );
    }

    #[test]
    fn many_channels_are_stored_and_searched_quickly() {
        let mut store = store();
        let id = source(&mut store, "Grande");
        let entries: Vec<Entry> = (0..50_000)
            .map(|i| Entry {
                name: format!("Canale {i}"),
                url: format!("http://h/live/u/p/{i}.ts"),
                group: Some(format!("Gruppo {}", i % 250)),
                ..Entry::default()
            })
            .collect();
        let start = std::time::Instant::now();
        store.replace_channels(id, &entries, &[], 1).unwrap();
        let stored = start.elapsed();
        let start = std::time::Instant::now();
        let hits = store.search(id, "canale 4999", 200).unwrap();
        let searched = start.elapsed();
        assert!(!hits.is_empty());
        // limiti larghi: valgono anche per le build di debug
        assert!(
            stored < std::time::Duration::from_secs(30),
            "scrittura: {stored:?}"
        );
        assert!(
            searched < std::time::Duration::from_millis(500),
            "ricerca: {searched:?}"
        );
    }
}
