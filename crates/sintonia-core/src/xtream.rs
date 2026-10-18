//! Client Xtream Codes (`player_api.php`).
//!
//! I server Xtream sono molto diversi tra loro: lo stesso campo arriva come numero, testo, `null`
//! o non arriva affatto, e gli elenchi possono contenere voci rotte. Qui ogni campo è letto in modo
//! tollerante e una voce illeggibile viene saltata, non fa fallire tutto l'elenco.
//!
//! Utente e password viaggiano nell'indirizzo (è il protocollo): gli errori di rete vengono sempre
//! ripuliti dall'URL, e [`Credentials`] non mostra la password nel `Debug`.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Dimensione massima di una risposta (i cataloghi grandi arrivano a decine di MB).
const MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

/// Errori del client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XtreamError {
    /// L'indirizzo del server non è valido.
    #[error("indirizzo del server non valido")]
    InvalidServer,
    /// Risposta HTTP non riuscita.
    #[error("il server ha risposto {0}")]
    Http(u16),
    /// Rete (testo senza URL).
    #[error("rete: {0}")]
    Network(String),
    /// Utente o password non accettati.
    #[error("utente o password non accettati")]
    Auth,
    /// La risposta non è JSON valido.
    #[error("risposta non valida: {0}")]
    Json(String),
    /// Risposta troppo grande.
    #[error("risposta troppo grande")]
    TooLarge,
}

/// Credenziali di un account.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    /// Indirizzo del server, già normalizzato (`http://host:porta`).
    pub server: String,
    /// Utente.
    pub username: String,
    /// Password.
    pub password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("server", &crate::redact::redact_url(&self.server))
            .finish_non_exhaustive()
    }
}

/// Normalizza l'indirizzo inserito dall'utente: aggiunge `http://` se manca, toglie la barra
/// finale e un eventuale `player_api.php` o `get.php?...` incollato per intero.
///
/// ```
/// use sintonia_core::xtream::normalize_server;
/// assert_eq!(normalize_server("tv.example.com:8080/").unwrap(), "http://tv.example.com:8080");
/// assert_eq!(
///     normalize_server("https://tv.example.com/player_api.php?username=a&password=b").unwrap(),
///     "https://tv.example.com"
/// );
/// ```
pub fn normalize_server(input: &str) -> Result<String, XtreamError> {
    let mut text = input.trim().to_owned();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return Err(XtreamError::InvalidServer);
    }
    if !text.contains("://") {
        text = format!("http://{text}");
    }
    let (scheme, rest) = text.split_once("://").ok_or(XtreamError::InvalidServer)?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(XtreamError::InvalidServer);
    }
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let mut path = rest.trim_end_matches('/').to_owned();
    for tail in [
        "/player_api.php",
        "/get.php",
        "/xmltv.php",
        "/panel_api.php",
    ] {
        if let Some(stripped) = path.strip_suffix(tail) {
            path = stripped.to_owned();
        }
    }
    let path = path.trim_end_matches('/');
    let host = path.split('/').next().unwrap_or_default();
    if host.is_empty() || host.contains('@') {
        return Err(XtreamError::InvalidServer);
    }
    Ok(format!("{scheme}://{path}"))
}

/// Codifica un valore per la query string.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Codifica un segmento di percorso (utente e password negli indirizzi dei flussi).
fn encode_path(value: &str) -> String {
    encode(value)
}

// ───────────── lettura tollerante ─────────────

mod lenient {
    use super::*;

    pub fn text(value: &Value) -> Option<String> {
        match value {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }

    pub fn int(value: &Value) -> Option<i64> {
        match value {
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            Value::String(s) => {
                let s = s.trim();
                s.parse::<i64>()
                    .ok()
                    .or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
            }
            Value::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    pub fn float(value: &Value) -> Option<f32> {
        match value {
            Value::Number(n) => n.as_f64().map(|f| f as f32),
            Value::String(s) => s.trim().replace(',', ".").parse().ok(),
            _ => None,
        }
        .filter(|f: &f32| f.is_finite())
    }

    pub fn flag(value: &Value) -> bool {
        int(value).is_some_and(|n| n != 0)
            || value
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("true"))
    }

    pub fn opt_text<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        Ok(text(&Value::deserialize(d)?))
    }
    pub fn opt_int<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
        Ok(int(&Value::deserialize(d)?))
    }
    pub fn opt_float<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
        Ok(float(&Value::deserialize(d)?))
    }
    pub fn as_flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
        Ok(flag(&Value::deserialize(d)?))
    }
}

/// Un elenco JSON in cui le voci illeggibili vengono saltate. Se al posto dell'elenco arriva
/// altro (oggetto vuoto, messaggio d'errore), l'elenco è vuoto.
fn lenient_list<T: DeserializeOwned>(value: Value) -> Vec<T> {
    let items = match value {
        Value::Array(items) => items,
        // alcuni server mandano un oggetto con chiavi numeriche invece di un elenco
        Value::Object(map) => map.into_iter().map(|(_, v)| v).collect(),
        _ => return Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value(item).ok())
        .collect()
}

// ───────────── dati ─────────────

/// Stato dell'account.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountInfo {
    /// Stato dichiarato dal server ("Active", "Expired", "Banned"…).
    pub status: String,
    /// Scadenza (secondi Unix), se indicata.
    pub expires: Option<i64>,
    /// Connessioni contemporanee ammesse.
    pub max_connections: Option<u32>,
    /// Connessioni attive adesso.
    pub active_connections: Option<u32>,
    /// Account di prova.
    pub trial: bool,
    /// Formati ammessi per i canali live ("m3u8", "ts"…).
    pub formats: Vec<String>,
}

impl AccountInfo {
    /// L'account è utilizzabile.
    pub fn is_active(&self) -> bool {
        self.status.eq_ignore_ascii_case("active")
    }

    /// Estensione da usare per i canali live: HLS se il server lo permette, altrimenti TS.
    pub fn live_extension(&self) -> &'static str {
        if self.formats.is_empty() || self.formats.iter().any(|f| f.eq_ignore_ascii_case("m3u8")) {
            "m3u8"
        } else {
            "ts"
        }
    }
}

/// Una categoria.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Category {
    /// Identificativo.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub category_id: Option<String>,
    /// Nome.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub category_name: Option<String>,
}

/// Un canale live.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LiveStream {
    /// Identificativo del flusso.
    #[serde(deserialize_with = "lenient::opt_int")]
    pub stream_id: Option<i64>,
    /// Nome.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub name: Option<String>,
    /// Numero del canale.
    #[serde(default, deserialize_with = "lenient::opt_int")]
    pub num: Option<i64>,
    /// Logo.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub stream_icon: Option<String>,
    /// Identificativo per la guida.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub epg_channel_id: Option<String>,
    /// Categoria.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub category_id: Option<String>,
    /// Il server registra il canale (catch-up, M3).
    #[serde(default, deserialize_with = "lenient::as_flag")]
    pub tv_archive: bool,
}

/// Un film.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Movie {
    /// Identificativo del flusso.
    #[serde(deserialize_with = "lenient::opt_int")]
    pub stream_id: Option<i64>,
    /// Titolo.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub name: Option<String>,
    /// Locandina.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub stream_icon: Option<String>,
    /// Categoria.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub category_id: Option<String>,
    /// Voto (di solito su 10).
    #[serde(default, deserialize_with = "lenient::opt_float")]
    pub rating: Option<f32>,
    /// Aggiunto il (secondi Unix).
    #[serde(default, deserialize_with = "lenient::opt_int")]
    pub added: Option<i64>,
    /// Estensione del file ("mp4", "mkv"…).
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub container_extension: Option<String>,
    /// Anno, se il server lo indica.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub year: Option<String>,
}

/// Una serie.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Series {
    /// Identificativo.
    #[serde(deserialize_with = "lenient::opt_int")]
    pub series_id: Option<i64>,
    /// Titolo.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub name: Option<String>,
    /// Locandina.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub cover: Option<String>,
    /// Categoria.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub category_id: Option<String>,
    /// Trama.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub plot: Option<String>,
    /// Genere.
    #[serde(default, deserialize_with = "lenient::opt_text")]
    pub genre: Option<String>,
    /// Data di uscita.
    #[serde(
        default,
        rename = "releaseDate",
        deserialize_with = "lenient::opt_text"
    )]
    pub release_date: Option<String>,
    /// Voto.
    #[serde(default, deserialize_with = "lenient::opt_float")]
    pub rating: Option<f32>,
}

/// Dettagli di un film (`get_vod_info`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MovieDetail {
    /// Trama.
    pub plot: Option<String>,
    /// Genere.
    pub genre: Option<String>,
    /// Regia.
    pub director: Option<String>,
    /// Interpreti.
    pub cast: Option<String>,
    /// Data di uscita.
    pub release_date: Option<String>,
    /// Durata in secondi.
    pub duration_secs: Option<u32>,
    /// Voto.
    pub rating: Option<f32>,
    /// Estensione del file.
    pub container: Option<String>,
}

/// Un episodio.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Episode {
    /// Identificativo del flusso.
    pub id: i64,
    /// Stagione.
    pub season: u32,
    /// Numero nella stagione.
    pub number: u32,
    /// Titolo.
    pub title: String,
    /// Estensione del file.
    pub container: String,
    /// Durata in secondi.
    pub duration_secs: Option<u32>,
    /// Trama.
    pub plot: Option<String>,
    /// Immagine.
    pub image: Option<String>,
}

/// Dettagli di una serie (`get_series_info`): episodi ordinati per stagione e numero.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesDetail {
    /// Trama.
    pub plot: Option<String>,
    /// Genere.
    pub genre: Option<String>,
    /// Episodi.
    pub episodes: Vec<Episode>,
}

// ───────────── client ─────────────

/// Il client di un account.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    credentials: Credentials,
}

impl Client {
    /// Client per un account; `credentials.server` deve essere già normalizzato.
    pub fn new(http: reqwest::Client, credentials: Credentials) -> Self {
        Self { http, credentials }
    }

    fn api_url(&self, action: Option<&str>, extra: &[(&str, String)]) -> String {
        let c = &self.credentials;
        let mut url = format!(
            "{}/player_api.php?username={}&password={}",
            c.server,
            encode(&c.username),
            encode(&c.password)
        );
        if let Some(action) = action {
            url.push_str("&action=");
            url.push_str(action);
        }
        for (key, value) in extra {
            url.push('&');
            url.push_str(key);
            url.push('=');
            url.push_str(&encode(value));
        }
        url
    }

    async fn get_json(
        &self,
        action: Option<&str>,
        extra: &[(&str, String)],
    ) -> Result<Value, XtreamError> {
        let network = |e: reqwest::Error| XtreamError::Network(e.without_url().to_string());
        let response = self
            .http
            .get(self.api_url(action, extra))
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .map_err(network)?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(XtreamError::Auth);
        }
        if !status.is_success() {
            return Err(XtreamError::Http(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
        {
            return Err(XtreamError::TooLarge);
        }
        let bytes = response.bytes().await.map_err(network)?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(XtreamError::TooLarge);
        }
        tokio::task::spawn_blocking(move || {
            // alcuni server rispondono con un corpo vuoto al posto di []
            if bytes.iter().all(u8::is_ascii_whitespace) {
                return Ok(Value::Null);
            }
            serde_json::from_slice::<Value>(&bytes).map_err(|e| XtreamError::Json(e.to_string()))
        })
        .await
        .map_err(|e| XtreamError::Json(e.to_string()))?
    }

    async fn list<T: DeserializeOwned + Send + 'static>(
        &self,
        action: &str,
    ) -> Result<Vec<T>, XtreamError> {
        let value = self.get_json(Some(action), &[]).await?;
        Ok(tokio::task::spawn_blocking(move || lenient_list(value))
            .await
            .unwrap_or_default())
    }

    /// Verifica l'account. `Err(Auth)` se utente o password non sono accettati.
    pub async fn account(&self) -> Result<AccountInfo, XtreamError> {
        let value = self.get_json(None, &[]).await?;
        parse_account(&value)
    }

    /// Categorie dei canali live.
    pub async fn live_categories(&self) -> Result<Vec<Category>, XtreamError> {
        self.list("get_live_categories").await
    }

    /// Canali live.
    pub async fn live_streams(&self) -> Result<Vec<LiveStream>, XtreamError> {
        self.list("get_live_streams").await
    }

    /// Categorie dei film.
    pub async fn movie_categories(&self) -> Result<Vec<Category>, XtreamError> {
        self.list("get_vod_categories").await
    }

    /// Film.
    pub async fn movies(&self) -> Result<Vec<Movie>, XtreamError> {
        self.list("get_vod_streams").await
    }

    /// Categorie delle serie.
    pub async fn series_categories(&self) -> Result<Vec<Category>, XtreamError> {
        self.list("get_series_categories").await
    }

    /// Serie.
    pub async fn series(&self) -> Result<Vec<Series>, XtreamError> {
        self.list("get_series").await
    }

    /// Dettagli di un film.
    pub async fn movie_detail(&self, stream_id: i64) -> Result<MovieDetail, XtreamError> {
        let value = self
            .get_json(Some("get_vod_info"), &[("vod_id", stream_id.to_string())])
            .await?;
        Ok(parse_movie_detail(&value))
    }

    /// Stagioni ed episodi di una serie.
    pub async fn series_detail(&self, series_id: i64) -> Result<SeriesDetail, XtreamError> {
        let value = self
            .get_json(
                Some("get_series_info"),
                &[("series_id", series_id.to_string())],
            )
            .await?;
        Ok(parse_series_detail(&value))
    }

    fn stream_url(&self, kind: &str, id: i64, extension: &str) -> String {
        let c = &self.credentials;
        let extension = extension.trim_start_matches('.');
        format!(
            "{}/{kind}/{}/{}/{id}.{extension}",
            c.server,
            encode_path(&c.username),
            encode_path(&c.password)
        )
    }

    /// Indirizzo di un canale live.
    pub fn live_url(&self, stream_id: i64, extension: &str) -> String {
        self.stream_url("live", stream_id, extension)
    }

    /// Indirizzo di un film.
    pub fn movie_url(&self, stream_id: i64, container: &str) -> String {
        self.stream_url("movie", stream_id, container)
    }

    /// Indirizzo di un episodio.
    pub fn episode_url(&self, episode_id: i64, container: &str) -> String {
        self.stream_url("series", episode_id, container)
    }
}

fn parse_account(value: &Value) -> Result<AccountInfo, XtreamError> {
    let Some(user) = value.get("user_info") else {
        return Err(XtreamError::Auth);
    };
    if user.get("auth").is_some_and(|a| !lenient::flag(a)) {
        return Err(XtreamError::Auth);
    }
    let formats = user
        .get("allowed_output_formats")
        .and_then(Value::as_array)
        .map(|f| f.iter().filter_map(lenient::text).collect())
        .unwrap_or_default();
    let count = |key: &str| {
        user.get(key)
            .and_then(lenient::int)
            .and_then(|n| u32::try_from(n).ok())
    };
    Ok(AccountInfo {
        status: user
            .get("status")
            .and_then(lenient::text)
            .unwrap_or_else(|| "Active".to_owned()),
        expires: user
            .get("exp_date")
            .and_then(lenient::int)
            .filter(|t| *t > 0),
        max_connections: count("max_connections"),
        active_connections: count("active_cons"),
        trial: user.get("is_trial").is_some_and(lenient::flag),
        formats,
    })
}

fn parse_movie_detail(value: &Value) -> MovieDetail {
    let info = value.get("info").unwrap_or(&Value::Null);
    let data = value.get("movie_data").unwrap_or(&Value::Null);
    let text = |v: &Value, key: &str| v.get(key).and_then(lenient::text);
    MovieDetail {
        plot: text(info, "plot").or_else(|| text(info, "description")),
        genre: text(info, "genre"),
        director: text(info, "director"),
        cast: text(info, "cast").or_else(|| text(info, "actors")),
        release_date: text(info, "releasedate").or_else(|| text(info, "release_date")),
        duration_secs: info
            .get("duration_secs")
            .and_then(lenient::int)
            .and_then(|d| u32::try_from(d).ok())
            .filter(|d| *d > 0),
        rating: info.get("rating").and_then(lenient::float),
        container: text(data, "container_extension"),
    }
}

fn parse_episode(value: &Value, season_hint: u32) -> Option<Episode> {
    let id = value.get("id").and_then(lenient::int)?;
    let info = value.get("info").unwrap_or(&Value::Null);
    let season = value
        .get("season")
        .and_then(lenient::int)
        .and_then(|s| u32::try_from(s).ok())
        .unwrap_or(season_hint);
    let number = value
        .get("episode_num")
        .and_then(lenient::int)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    Some(Episode {
        id,
        season,
        number,
        title: value
            .get("title")
            .and_then(lenient::text)
            .unwrap_or_default(),
        container: value
            .get("container_extension")
            .and_then(lenient::text)
            .unwrap_or_else(|| "mp4".to_owned()),
        duration_secs: info
            .get("duration_secs")
            .and_then(lenient::int)
            .and_then(|d| u32::try_from(d).ok())
            .filter(|d| *d > 0),
        plot: info.get("plot").and_then(lenient::text),
        image: info.get("movie_image").and_then(lenient::text),
    })
}

fn parse_series_detail(value: &Value) -> SeriesDetail {
    let info = value.get("info").unwrap_or(&Value::Null);
    let mut episodes = Vec::new();
    match value.get("episodes") {
        // forma comune: { "1": [ … ], "2": [ … ] }
        Some(Value::Object(seasons)) => {
            for (season, list) in seasons {
                let hint = season.parse().unwrap_or(0);
                if let Some(list) = list.as_array() {
                    episodes.extend(list.iter().filter_map(|e| parse_episode(e, hint)));
                }
            }
        }
        // varianti: elenco di elenchi, o elenco piatto
        Some(Value::Array(items)) => {
            for item in items {
                match item {
                    Value::Array(list) => {
                        episodes.extend(list.iter().filter_map(|e| parse_episode(e, 0)));
                    }
                    other => episodes.extend(parse_episode(other, 0)),
                }
            }
        }
        _ => {}
    }
    episodes.sort_by_key(|e| (e.season, e.number, e.id));
    SeriesDetail {
        plot: info.get("plot").and_then(lenient::text),
        genre: info.get("genre").and_then(lenient::text),
        episodes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn server_addresses_are_normalized() {
        assert_eq!(normalize_server("http://h:80//").unwrap(), "http://h:80");
        assert_eq!(normalize_server("h/sub/").unwrap(), "http://h/sub");
        assert_eq!(
            normalize_server("HTTPS://h/get.php?username=x").unwrap(),
            "https://h"
        );
        assert!(normalize_server("ftp://h").is_err());
        assert!(normalize_server("http://u:p@h").is_err());
        assert!(normalize_server("  ").is_err());
    }

    #[test]
    fn urls_encode_credentials() {
        let client = Client::new(
            reqwest::Client::new(),
            Credentials {
                server: "http://h:8080".into(),
                username: "mario rossi".into(),
                password: "p&ss/1".into(),
            },
        );
        assert_eq!(
            client.api_url(Some("get_live_streams"), &[]),
            "http://h:8080/player_api.php?username=mario%20rossi&password=p%26ss%2F1&action=get_live_streams"
        );
        assert_eq!(
            client.live_url(7, "m3u8"),
            "http://h:8080/live/mario%20rossi/p%26ss%2F1/7.m3u8"
        );
        assert_eq!(
            client.movie_url(9, ".mkv"),
            "http://h:8080/movie/mario%20rossi/p%26ss%2F1/9.mkv"
        );
        assert!(!format!("{:?}", client).contains("p&ss"));
    }

    #[test]
    fn account_variants() {
        let value = json!({"user_info": {"auth": 1, "status": "Active", "exp_date": "1893456000",
            "max_connections": "2", "active_cons": 0, "is_trial": "0",
            "allowed_output_formats": ["ts"]}});
        let account = parse_account(&value).unwrap();
        assert!(account.is_active());
        assert_eq!(account.expires, Some(1_893_456_000));
        assert_eq!(
            (account.max_connections, account.active_connections),
            (Some(2), Some(0))
        );
        assert_eq!(account.live_extension(), "ts");
        assert_eq!(
            parse_account(&json!({"user_info": {"auth": 0}})),
            Err(XtreamError::Auth)
        );
        assert_eq!(parse_account(&json!([])), Err(XtreamError::Auth));
        let expired =
            parse_account(&json!({"user_info": {"status": "Expired", "exp_date": null}})).unwrap();
        assert!(!expired.is_active());
        assert_eq!(expired.expires, None);
        assert_eq!(expired.live_extension(), "m3u8");
    }

    #[test]
    fn lists_skip_broken_items_and_mixed_types() {
        let value = json!([
            {"stream_id": "12", "name": "Uno", "num": 1, "category_id": 3, "tv_archive": "1"},
            {"stream_id": 13.0, "name": null, "stream_icon": ""},
            {"name": "senza id"},
            "spazzatura",
            {"stream_id": "x"}
        ]);
        let streams: Vec<LiveStream> = lenient_list(value);
        // senza il campo stream_id la voce è scartata; con un valore illeggibile resta con None
        // (la scarta chi la usa)
        let ids: Vec<Option<i64>> = streams.iter().map(|s| s.stream_id).collect();
        assert_eq!(ids, [Some(12), Some(13), None]);
        assert_eq!(streams[0].category_id.as_deref(), Some("3"));
        assert!(streams[0].tv_archive);
        assert_eq!(streams[1].stream_icon, None);
        assert!(lenient_list::<LiveStream>(json!({})).is_empty());
        assert!(lenient_list::<LiveStream>(Value::Null).is_empty());
    }

    #[test]
    fn movies_with_string_ratings() {
        let movies: Vec<Movie> = lenient_list(json!([
            {"stream_id": 1, "name": "A", "rating": "7,5", "container_extension": "mkv"},
            {"stream_id": 2, "name": "B", "rating": "", "added": "1700000000"}
        ]));
        assert_eq!(movies[0].rating, Some(7.5));
        assert_eq!(movies[1].rating, None);
        assert_eq!(movies[1].added, Some(1_700_000_000));
    }

    #[test]
    fn series_detail_shapes() {
        let by_season = json!({"info": {"plot": "Trama"}, "episodes": {
            "2": [{"id": "21", "episode_num": "1", "title": "S2E1", "container_extension": "mkv"}],
            "1": [{"id": 12, "episode_num": 2, "title": "S1E2", "info": {"duration_secs": "1500"}},
                  {"id": 11, "episode_num": 1, "title": "S1E1"}]
        }});
        let detail = parse_series_detail(&by_season);
        let order: Vec<(u32, u32, i64)> = detail
            .episodes
            .iter()
            .map(|e| (e.season, e.number, e.id))
            .collect();
        assert_eq!(order, [(1, 1, 11), (1, 2, 12), (2, 1, 21)]);
        assert_eq!(detail.episodes[1].duration_secs, Some(1500));
        assert_eq!(detail.episodes[0].container, "mp4");
        assert_eq!(detail.plot.as_deref(), Some("Trama"));

        let flat = json!({"episodes": [[{"id": 1, "season": 3, "episode_num": 1}], {"id": 2, "season": 3, "episode_num": 2}]});
        assert_eq!(parse_series_detail(&flat).episodes.len(), 2);
        assert!(parse_series_detail(&json!({"episodes": null}))
            .episodes
            .is_empty());
    }

    #[test]
    fn movie_detail_fields() {
        let detail = parse_movie_detail(&json!({
            "info": {"plot": "P", "duration_secs": 5400, "rating": "8", "releasedate": "2020-01-01"},
            "movie_data": {"container_extension": "mp4"}
        }));
        assert_eq!(detail.duration_secs, Some(5400));
        assert_eq!(detail.rating, Some(8.0));
        assert_eq!(detail.container.as_deref(), Some("mp4"));
        assert_eq!(parse_movie_detail(&Value::Null), MovieDetail::default());
    }
}
