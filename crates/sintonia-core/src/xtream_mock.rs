//! Finto server Xtream Codes, solo per i test e lo sviluppo (feature `mock`).
//!
//! Risponde a `player_api.php` con un catalogo **inventato** (nessun canale reale), a
//! `xmltv.php` con una guida inventata attorno all'ora attuale e, per i flussi, rimanda con un
//! redirect a stream pubblici di prova. Accetta un solo utente:
//! [`USERNAME`] / [`PASSWORD`]. Volutamente "sporco" come i server veri: numeri come testo,
//! campi nulli, una voce rotta in ogni elenco.

use std::fmt::Write as _;
use std::net::SocketAddr;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Utente accettato.
pub const USERNAME: &str = "mario";
/// Password accettata.
pub const PASSWORD: &str = "prova123";

/// Flussi pubblici di prova a cui rimandano canali, film ed episodi.
const LIVE_STREAM: &str = "https://demo.unified-streaming.com/k8s/live/stable/live.isml/.m3u8";
const MOVIE_STREAMS: [&str; 2] = [
    "https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8",
    "https://demo.unified-streaming.com/k8s/features/stable/video/tears-of-steel/tears-of-steel.ism/.m3u8",
];

/// Il catalogo servito.
#[derive(Debug, Clone)]
pub struct Catalog {
    live: usize,
    movies: usize,
    series: usize,
}

impl Catalog {
    /// Catalogo piccolo, per i test e per guardarlo a occhio.
    pub fn sample() -> Self {
        Self {
            live: 40,
            movies: 60,
            series: 12,
        }
    }

    /// Catalogo grande, per provare le prestazioni.
    pub fn large(live: usize, movies: usize, series: usize) -> Self {
        Self {
            live,
            movies,
            series,
        }
    }

    fn live_categories() -> Value {
        json!([
            {"category_id": "1", "category_name": "Notizie", "parent_id": 0},
            {"category_id": "2", "category_name": "Sport", "parent_id": 0},
            {"category_id": 3, "category_name": "Intrattenimento", "parent_id": 0},
            {"category_id": "4", "category_name": "Musica"}
        ])
    }

    fn live_streams(&self) -> Value {
        let mut items: Vec<Value> = (1..=self.live)
            .map(|i| {
                json!({
                    "num": i,
                    "name": format!("Canale di prova {i}"),
                    "stream_type": "live",
                    // alternati numero e testo, come fanno i server veri
                    "stream_id": if i % 2 == 0 { json!(1000 + i) } else { json!((1000 + i).to_string()) },
                    "stream_icon": "",
                    "epg_channel_id": format!("prova{i}.it"),
                    "category_id": ((i - 1) % 4 + 1).to_string(),
                    "tv_archive": if i % 5 == 0 { 1 } else { 0 },
                })
            })
            .collect();
        items.push(json!({"name": "voce rotta senza id"}));
        Value::Array(items)
    }

    fn movie_categories() -> Value {
        json!([
            {"category_id": "10", "category_name": "Azione"},
            {"category_id": "11", "category_name": "Commedia"},
            {"category_id": "12", "category_name": "Documentari"}
        ])
    }

    fn movie_title(i: usize) -> String {
        const A: [&str; 8] = [
            "Il segreto",
            "La notte",
            "Ultima",
            "Il ritorno",
            "La strada",
            "Il faro",
            "Codice",
            "Onde",
        ];
        const B: [&str; 6] = [
            "del nord",
            "di vetro",
            "d'inverno",
            "perduta",
            "lontana",
            "di carta",
        ];
        format!("{} {} {}", A[i % A.len()], B[(i / A.len()) % B.len()], i)
    }

    fn movies(&self) -> Value {
        let mut items: Vec<Value> = (1..=self.movies)
            .map(|i| {
                json!({
                    "num": i,
                    "name": Self::movie_title(i),
                    "stream_type": "movie",
                    "stream_id": 5000 + i,
                    "stream_icon": null,
                    "rating": if i % 3 == 0 { json!("") } else { json!(format!("{}.{}", 5 + i % 4, i % 10)) },
                    "added": (1_700_000_000 + i * 3600).to_string(),
                    "category_id": (10 + i % 3).to_string(),
                    "container_extension": if i % 2 == 0 { "mkv" } else { "mp4" },
                    "year": (1990 + i % 34).to_string(),
                })
            })
            .collect();
        items.push(json!("spazzatura"));
        Value::Array(items)
    }

    fn series_categories() -> Value {
        json!([
            {"category_id": "20", "category_name": "Drammatiche"},
            {"category_id": "21", "category_name": "Commedie"}
        ])
    }

    fn series(&self) -> Value {
        Value::Array(
            (1..=self.series)
                .map(|i| {
                    json!({
                        "num": i,
                        "name": format!("Serie di prova {i}"),
                        "series_id": 800 + i,
                        "cover": "",
                        "plot": format!("Trama inventata della serie {i}."),
                        "genre": if i % 2 == 0 { "Commedia" } else { "Dramma" },
                        "releaseDate": format!("{}-03-01", 2000 + i),
                        "rating": (6 + i % 4).to_string(),
                        "category_id": (20 + i % 2).to_string(),
                    })
                })
                .collect(),
        )
    }

    fn series_info(&self, series_id: i64) -> Value {
        let index = usize::try_from(series_id - 800).unwrap_or(0);
        if index == 0 || index > self.series {
            return json!({"seasons": [], "info": {}, "episodes": []});
        }
        let seasons = 1 + index % 3;
        let mut episodes = serde_json::Map::new();
        for season in 1..=seasons {
            let list: Vec<Value> = (1..=4 + season)
                .map(|n| {
                    json!({
                        "id": (series_id * 100 + i64::try_from(season * 10 + n).unwrap_or(0)).to_string(),
                        "episode_num": n,
                        "title": format!("Episodio {n}"),
                        "container_extension": "mp4",
                        "season": season,
                        "info": {"duration_secs": 1500 + n * 60, "plot": format!("Episodio {n} della stagione {season}.")},
                    })
                })
                .collect();
            episodes.insert(season.to_string(), Value::Array(list));
        }
        json!({
            "info": {"name": format!("Serie di prova {index}"), "plot": format!("Trama inventata della serie {index}."), "genre": "Dramma"},
            "episodes": episodes,
        })
    }

    /// Guida XMLTV: da ieri a tre giorni da oggi (UTC), al massimo per 2000 canali.
    fn xmltv(&self, now: i64) -> String {
        const TITLES: [&str; 10] = [
            "Telegiornale",
            "Meteo & traffico",
            "Quiz serale",
            "Documentario: oceani",
            "Notte musicale",
            "Rubrica di cucina",
            "Talk show",
            "Cartoni animati",
            "Approfondimento",
            "Sport: sintesi",
        ];
        const MINUTES: [i64; 6] = [30, 60, 45, 90, 30, 120];
        let day_start = now - now.rem_euclid(86_400);
        let (from, to) = (day_start - 86_400, day_start + 3 * 86_400);
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<tv generator-info-name=\"finto\">
",
        );
        let channels = self.live.min(2000);
        for i in 1..=channels {
            let _ = writeln!(
                xml,
                "<channel id=\"prova{i}.it\"><display-name>Canale di prova {i}</display-name></channel>"
            );
        }
        for i in 1..=channels {
            let mut start = from;
            let mut n = i;
            while start < to {
                let length = MINUTES[n % MINUTES.len()] * 60;
                let title = if n % 7 == 3 {
                    format!("Film: {}", Self::movie_title(n))
                } else {
                    TITLES[n % TITLES.len()].replace('&', "&amp;")
                };
                // un programma ogni tanto senza «stop», come in certe guide vere
                let stop = if n % 50 == 0 {
                    String::new()
                } else {
                    format!(" stop=\"{}\"", xmltv_time(start + length))
                };
                let _ = writeln!(
                    xml,
                    "<programme start=\"{}\"{stop} channel=\"prova{i}.it\"><title lang=\"it\">{title}</title><desc>Programma inventato numero {n}.</desc><category>Prova</category></programme>",
                    xmltv_time(start)
                );
                start += length;
                n += 1;
            }
        }
        xml.push_str(
            "</tv>
",
        );
        xml
    }

    fn movie_info(stream_id: i64) -> Value {
        json!({
            "info": {
                "plot": format!("Trama inventata del film {stream_id}."),
                "genre": "Avventura",
                "director": "Regista Inventato",
                "cast": "Attrice Uno, Attore Due",
                "releasedate": "2019-10-01",
                "duration_secs": 5400,
                "rating": "7.2"
            },
            "movie_data": {"stream_id": stream_id, "container_extension": "mp4"}
        })
    }
}

/// Il server in esecuzione (si ferma con il processo).
#[derive(Debug)]
pub struct MockServer {
    address: SocketAddr,
}

impl MockServer {
    /// Avvia il server su una porta libera di `127.0.0.1`.
    pub fn start(catalog: Catalog) -> Self {
        Self::start_on(catalog, 0)
    }

    /// Avvia il server su una porta precisa (0 = qualsiasi).
    ///
    /// # Panics
    /// Se la porta non è disponibile: è uno strumento di sviluppo.
    pub fn start_on(catalog: Catalog, port: u16) -> Self {
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", port))
            .expect("porta del finto server Xtream non disponibile");
        std_listener
            .set_nonblocking(true)
            .expect("socket non bloccante");
        let address = std_listener
            .local_addr()
            .expect("indirizzo del finto server");
        std::thread::Builder::new()
            .name("finto-xtream".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime del finto server");
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(std_listener)
                        .expect("listener del finto server");
                    loop {
                        let Ok((socket, _)) = listener.accept().await else {
                            continue;
                        };
                        let catalog = catalog.clone();
                        tokio::spawn(async move { serve(socket, &catalog).await });
                    }
                });
            })
            .expect("thread del finto server");
        Self { address }
    }

    /// Indirizzo da inserire come server (`http://127.0.0.1:porta`).
    pub fn address(&self) -> String {
        format!("http://{}", self.address)
    }
}

/// Orario XMLTV in UTC (`20261017203000 +0000`).
fn xmltv_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // da giorni dal 1970 a data (algoritmo di Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02} +0000",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
}

async fn serve(mut socket: tokio::net::TcpStream, catalog: &Catalog) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") {
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
        if buffer.len() > 64 * 1024 {
            return;
        }
    }
    let request = String::from_utf8_lossy(&buffer);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let response = route(catalog, path, query);
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

fn response(status: &str, headers: &str, body: &str) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    out
}

fn route(catalog: &Catalog, path: &str, query: &str) -> String {
    if path == "/player_api.php" {
        let authorized = query_param(query, "username").as_deref() == Some(USERNAME)
            && query_param(query, "password").as_deref() == Some(PASSWORD);
        let body = if !authorized {
            json!({"user_info": {"auth": 0}})
        } else {
            match query_param(query, "action").as_deref() {
                None => json!({
                    "user_info": {"username": USERNAME, "auth": 1, "status": "Active",
                        "exp_date": "1924992000", "is_trial": "0", "active_cons": "0",
                        "max_connections": "1", "allowed_output_formats": ["m3u8", "ts"]},
                    "server_info": {"url": "127.0.0.1", "server_protocol": "http"}
                }),
                Some("get_live_categories") => Catalog::live_categories(),
                Some("get_live_streams") => catalog.live_streams(),
                Some("get_vod_categories") => Catalog::movie_categories(),
                Some("get_vod_streams") => catalog.movies(),
                Some("get_series_categories") => Catalog::series_categories(),
                Some("get_series") => catalog.series(),
                Some("get_series_info") => {
                    let id = query_param(query, "series_id")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    catalog.series_info(id)
                }
                Some("get_vod_info") => {
                    let id = query_param(query, "vod_id")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    Catalog::movie_info(id)
                }
                Some(_) => json!([]),
            }
        };
        return response(
            "200 OK",
            "Content-Type: application/json\r\n",
            &body.to_string(),
        );
    }
    if path == "/xmltv.php" {
        let authorized = query_param(query, "username").as_deref() == Some(USERNAME)
            && query_param(query, "password").as_deref() == Some(PASSWORD);
        if !authorized {
            return response("403 Forbidden", "", "");
        }
        return response(
            "200 OK",
            "Content-Type: application/xml
",
            &catalog.xmltv(now()),
        );
    }
    // flussi: /live|movie|series/<utente>/<password>/<id>.<estensione>
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if let [kind, user, pass, file] = parts.as_slice() {
        if percent_decode(user) != USERNAME || percent_decode(pass) != PASSWORD {
            return response("403 Forbidden", "", "");
        }
        let id: usize = file
            .split('.')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let target = match *kind {
            "live" => LIVE_STREAM,
            "movie" | "series" => MOVIE_STREAMS[id % MOVIE_STREAMS.len()],
            _ => return response("404 Not Found", "", ""),
        };
        return response("302 Found", &format!("Location: {target}\r\n"), "");
    }
    response("404 Not Found", "", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        let catalog = Catalog::sample();
        let ok = route(
            &catalog,
            "/player_api.php",
            "username=mario&password=prova123&action=get_live_streams",
        );
        assert!(ok.starts_with("HTTP/1.1 200") && ok.contains("Canale di prova 1"));
        let denied = route(&catalog, "/player_api.php", "username=mario&password=no");
        assert!(denied.contains("\"auth\":0"));
        let stream = route(&catalog, "/movie/mario/prova123/5001.mp4", "");
        assert!(stream.starts_with("HTTP/1.1 302") && stream.contains("Location: https://"));
        assert!(route(&catalog, "/live/mario/sbagliata/1.ts", "").starts_with("HTTP/1.1 403"));
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        let guide = route(&catalog, "/xmltv.php", "username=mario&password=prova123");
        assert!(guide.contains("<programme") && guide.contains("&amp;"));
        assert!(route(&catalog, "/xmltv.php", "username=x&password=y").starts_with("HTTP/1.1 403"));
    }

    #[test]
    fn guide_times_are_valid() {
        assert_eq!(xmltv_time(0), "19700101000000 +0000");
        assert_eq!(xmltv_time(1_792_886_400), "20261025000000 +0000");
        let guide = Catalog::sample().xmltv(1_792_886_400);
        let mut count = 0;
        crate::epg::parse(guide.as_bytes(), (0, i64::MAX), &mut |_| {}, &mut |_| {
            count += 1
        })
        .expect("guida del finto server leggibile");
        assert!(
            count > 40 * 4 * 10,
            "almeno dieci programmi al giorno per canale: {count}"
        );
    }
}
