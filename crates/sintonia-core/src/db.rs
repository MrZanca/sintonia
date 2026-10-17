//! Archivio SQLite: sorgenti, canali, impostazioni, ricerca full-text (FTS5).
//!
//! Gli indirizzi (della playlist e di ogni canale) sono cifrati con [`Cipher`]: possono contenere
//! utente e password del fornitore. In chiaro restano solo i dati che servono a mostrare e cercare
//! i canali (nome, categoria, logo).

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use crate::crypto::{Cipher, CryptoError};
use crate::m3u::Entry;
use crate::redact::redact_url;

const SCHEMA_VERSION: i32 = 1;

/// Errori dell'archivio.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Errore di SQLite.
    #[error("archivio: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Un valore cifrato non si decifra.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// La sorgente o il canale non esiste più.
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
}

impl SourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::M3uUrl => "m3u-url",
            Self::M3uFile => "m3u-file",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "m3u-url" => Some(Self::M3uUrl),
            "m3u-file" => Some(Self::M3uFile),
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
    /// Ultimo aggiornamento (secondi Unix).
    pub refreshed_at: Option<i64>,
}

/// Dati per creare una sorgente.
#[derive(Clone, PartialEq, Eq)]
pub struct NewSource {
    /// Nome.
    pub name: String,
    /// Tipo.
    pub kind: SourceKind,
    /// Indirizzo o percorso (sensibile).
    pub location: String,
    /// User-agent per la playlist.
    pub user_agent: Option<String>,
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

/// Un canale nell'elenco (senza indirizzo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRow {
    /// Identificativo (cambia quando la sorgente si aggiorna).
    pub id: i64,
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

/// Quello che serve al player per aprire un canale.
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

/// L'archivio.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    cipher: Cipher,
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
            conn.execute_batch(
                "BEGIN;
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
                COMMIT;",
            )?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn, cipher })
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
        };
        let location = self.cipher.encrypt(&source.location)?;
        let tx = self.conn.transaction()?;
        let has_active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sources WHERE active = 1)",
            [],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO sources(name, kind, location, display, user_agent, active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                source.name,
                source.kind.as_str(),
                location,
                display,
                source.user_agent,
                !has_active
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Tutte le sorgenti, in ordine di creazione.
    pub fn sources(&self) -> Result<Vec<SourceInfo>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, display, user_agent, active, channel_count, refreshed_at
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
                refreshed_at: r.get(7)?,
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

    /// Indirizzo o percorso in chiaro, con il tipo e l'eventuale user-agent.
    pub fn source_location(&self, id: i64) -> Result<NewSource, StoreError> {
        let (name, kind, location, user_agent): (String, String, Vec<u8>, Option<String>) = self
            .conn
            .query_row(
                "SELECT name, kind, location, user_agent FROM sources WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        Ok(NewSource {
            name,
            kind: SourceKind::parse(&kind).unwrap_or(SourceKind::M3uUrl),
            location: self.cipher.decrypt(&location)?,
            user_agent,
        })
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
        let found = tx.execute("UPDATE sources SET active = (id = ?1)", [id])?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sources WHERE id = ?1)",
            [id],
            |r| r.get(0),
        )?;
        if found == 0 || !exists {
            return Err(StoreError::NotFound);
        }
        tx.commit()?;
        Ok(())
    }

    /// Elimina una sorgente e i suoi canali. Se era in uso passa alla prima rimasta.
    pub fn delete_source(&mut self, id: i64) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM sources WHERE id = ?1", [id])?;
        tx.execute(
            "UPDATE sources SET active = 1
             WHERE id = (SELECT MIN(id) FROM sources)
               AND NOT EXISTS(SELECT 1 FROM sources WHERE active = 1)",
            [],
        )?;
        tx.execute(
            "INSERT INTO channels_fts(channels_fts) VALUES ('rebuild')",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Sostituisce i canali di una sorgente con quelli appena letti.
    pub fn replace_channels(
        &mut self,
        source_id: i64,
        entries: &[Entry],
        epg_urls: &[String],
        now: i64,
    ) -> Result<u32, StoreError> {
        let epg = if epg_urls.is_empty() {
            None
        } else {
            Some(self.cipher.encrypt(&epg_urls.join("\n"))?)
        };
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM channels WHERE source_id = ?1", [source_id])?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO channels(source_id, position, number, name, group_name, tvg_id,
                    tvg_name, logo, url, user_agent, referrer, radio, drm)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?;
            for (position, entry) in entries.iter().enumerate() {
                let position = u32::try_from(position).unwrap_or(u32::MAX);
                insert.execute(params![
                    source_id,
                    position,
                    entry.number.unwrap_or(position.saturating_add(1)),
                    entry.name,
                    entry.group.as_deref().unwrap_or_default(),
                    entry.tvg_id,
                    entry.tvg_name,
                    entry.logo,
                    self.cipher.encrypt(&entry.url)?,
                    entry.user_agent,
                    entry.referrer,
                    entry.radio,
                    entry.drm,
                ])?;
            }
        }
        let count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
        let changed = tx.execute(
            "UPDATE sources SET channel_count = ?2, refreshed_at = ?3, epg_urls = ?4 WHERE id = ?1",
            params![source_id, count, now, epg],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        tx.execute(
            "INSERT INTO channels_fts(channels_fts) VALUES ('rebuild')",
            [],
        )?;
        tx.commit()?;
        Ok(count)
    }

    // ───────────── canali ─────────────

    /// I canali di una sorgente nell'ordine della playlist.
    pub fn channels(&self, source_id: i64) -> Result<Vec<ChannelRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, number, name, group_name, logo, tvg_id, radio, drm
             FROM channels WHERE source_id = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map([source_id], |r| {
            Ok(ChannelRow {
                id: r.get(0)?,
                number: r.get(1)?,
                name: r.get(2)?,
                group: r.get(3)?,
                logo: r.get(4)?,
                tvg_id: r.get(5)?,
                radio: r.get(6)?,
                drm: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Indirizzo (decifrato) e intestazioni di un canale.
    pub fn stream(&self, channel_id: i64) -> Result<StreamInfo, StoreError> {
        let (url, user_agent, referrer): (Vec<u8>, Option<String>, Option<String>) = self
            .conn
            .query_row(
                "SELECT url, user_agent, referrer FROM channels WHERE id = ?1",
                [channel_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        Ok(StreamInfo {
            url: self.cipher.decrypt(&url)?,
            user_agent,
            referrer,
        })
    }

    /// Ricerca istantanea su nome e categoria nella sorgente: ogni parola è un prefisso
    /// ("rai sp" trova "Rai Sport"). Accenti e maiuscole non contano.
    pub fn search(&self, source_id: i64, query: &str, limit: u32) -> Result<Vec<i64>, StoreError> {
        let Some(expr) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            "SELECT c.id FROM channels_fts f JOIN channels c ON c.id = f.rowid
             WHERE channels_fts MATCH ?1 AND c.source_id = ?2
             ORDER BY c.position LIMIT ?3",
        )?;
        let ids = stmt.query_map(params![expr, source_id, limit], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    // ───────────── impostazioni ─────────────

    /// Tutte le impostazioni salvate.
    pub fn settings(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM settings ORDER BY key")?;
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
                user_agent: None,
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
    fn channels_round_trip_with_encrypted_urls() {
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
            .replace_channels(a, &list.entries[..1], &[], 2)
            .unwrap();
        assert_eq!(store.channels(a).unwrap().len(), 1);
        store.delete_source(a).unwrap();
        assert!(store.channels(a).unwrap().is_empty());
        assert_eq!(store.active_source().unwrap(), Some(b));
        assert!(store.search(b, "rai", 5).unwrap().is_empty());
        assert!(matches!(
            store.set_active_source(a),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn settings_upsert() {
        let store = store();
        store.set_setting("lingua", "it").unwrap();
        store.set_setting("lingua", "en").unwrap();
        assert_eq!(store.setting("lingua").unwrap().as_deref(), Some("en"));
        assert_eq!(store.settings().unwrap().len(), 1);
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
