//! Prova di carico del servizio, senza interfaccia e senza dati veri: playlist, guida e catalogo
//! Xtream generati (inventati), in una cartella temporanea, con un portachiavi in memoria.
//!
//! ```text
//! cargo run --release -p sintonia-core --features mock --example stress -- [canali] [programmi]
//! ```
//!
//! Prove: playlist M3U grande e sporca, guida XMLTV `.gz` grande, ricerche, catalogo Xtream grande,
//! raffica di comandi concorrenti, file rotti, chiusure forzate durante le scritture e due processi
//! sullo stesso archivio. Alla fine l'archivio deve passare `PRAGMA integrity_check`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, ExitCode};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sintonia_core::crypto::Cipher;
use sintonia_core::db::{HistoryEntry, ItemKind, NewSource, SourceKind, Store};
use sintonia_core::secrets::MemorySecrets;
use sintonia_core::service::{Command, EpgState, Event, Service, ServiceConfig};
use sintonia_core::xtream_mock::{Catalog, MockServer, PASSWORD, USERNAME};

/// Chiave fissa: i processi figli devono aprire lo stesso archivio.
const KEY: [u8; 32] = [7; 32];

type Res<T> = Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // processo figlio: scrive nell'archivio finché non viene ucciso (o per `secs` secondi)
    if args.first().map(String::as_str) == Some("--scrittore") {
        let path = PathBuf::from(&args[1]);
        let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3600);
        return match writer(&path, secs) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("scrittore: {err}");
                ExitCode::FAILURE
            }
        };
    }
    let channels: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(50_000);
    let programmes: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(500_000);
    match run(channels, programmes) {
        Ok(()) => {
            println!("\nTUTTO OK");
            ExitCode::SUCCESS
        }
        Err(err) => {
            println!("\nFALLITO: {err}");
            ExitCode::FAILURE
        }
    }
}

struct Harness {
    service: Service,
    events: Receiver<Event>,
}

impl Harness {
    fn start(dir: &Path) -> Res<Self> {
        let (tx, events) = mpsc::channel();
        let service = Service::start(
            ServiceConfig {
                database: dir.join("archivio.db"),
                cache_dir: dir.join("cache"),
                cipher: Cipher::new(&KEY),
                secrets: Arc::new(MemorySecrets::default()),
                user_agent: "Sintonia-stress".into(),
            },
            move |e| {
                let _ = tx.send(e);
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { service, events })
    }

    /// Aspetta un evento che soddisfa `pick`, scartando gli altri; gli errori del servizio
    /// fanno fallire la prova.
    fn wait<T>(&self, secs: u64, mut pick: impl FnMut(Event) -> Option<T>) -> Res<T> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(Event::Error(e)) => return Err(format!("errore del servizio: {e}")),
                Ok(event) => {
                    if let Some(found) = pick(event) {
                        return Ok(found);
                    }
                }
                Err(_) => return Err(format!("nessuna risposta in {secs} s")),
            }
        }
    }
}

fn step(name: &str) -> Instant {
    print!("{name:.<58} ");
    let _ = std::io::stdout().flush();
    Instant::now()
}

fn done(started: Instant, detail: &str) {
    println!("ok  {:>7.2} s  {detail}", started.elapsed().as_secs_f64());
}

fn run(channels: usize, programmes: usize) -> Res<()> {
    let dir = std::env::temp_dir().join(format!("sintonia-stress-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    println!("cartella di prova: {}", dir.display());
    let result = run_in(&dir, channels, programmes);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run_in(dir: &Path, channels: usize, programmes: usize) -> Res<()> {
    let h = Harness::start(dir)?;
    h.service.send(Command::Init);
    h.wait(30, |e| matches!(e, Event::Catalog { .. }).then_some(()))?;

    // 1. playlist M3U grande e sporca
    let t = step(&format!(
        "genera playlist ({channels} canali, righe sporche)"
    ));
    let playlist = dir.join("lista.m3u");
    let expected = write_playlist(&playlist, channels)?;
    done(t, &format!("{} MB", file_mb(&playlist)));

    let t = step("importa la playlist");
    h.service.send(Command::AddSource(NewSource {
        name: "Prova".into(),
        kind: SourceKind::M3uFile,
        location: playlist.display().to_string(),
        username: None,
        user_agent: None,
        epg_url: None,
    }));
    let (source_id, report) = h.wait(300, |e| match e {
        Event::SourceSaved { source_id, result } => Some((source_id, result)),
        _ => None,
    })?;
    let report = report.map_err(|e| format!("playlist: {e}"))?;
    let source_id = source_id.ok_or("sorgente non salvata")?;
    if report.channels != expected {
        return Err(format!(
            "canali letti {} invece di {expected}",
            report.channels
        ));
    }
    done(
        t,
        &format!(
            "{} canali, {} righe saltate",
            report.channels, report.skipped
        ),
    );

    // 2. guida grande
    let t = step(&format!("genera guida .gz ({programmes} programmi)"));
    let guide = dir.join("guida.xml.gz");
    write_guide(&guide, channels.min(3000), programmes)?;
    done(t, &format!("{} MB compressi", file_mb(&guide)));

    let t = step("importa la guida");
    h.service.send(Command::SetSourceEpg {
        source_id,
        epg_url: Some(guide.display().to_string()),
    });
    let summary = h.wait(900, |e| match e {
        Event::Epg {
            state: EpgState::Done(result),
            ..
        } => Some(result),
        _ => None,
    })?;
    let summary = summary.map_err(|e| format!("guida: {e}"))?;
    done(
        t,
        &format!(
            "{} programmi, {} canali abbinati",
            summary.programmes, summary.matched
        ),
    );

    let t = step("carica 28 ore di guida (come «in onda / dopo»)");
    let now = now();
    h.service.send(Command::LoadGuide {
        from: now - 4 * 3600,
        to: now + 24 * 3600,
        generation: 1,
    });
    let loaded = h.wait(120, |e| match e {
        Event::Guide { programmes, .. } => Some(programmes.len()),
        _ => None,
    })?;
    done(t, &format!("{loaded} programmi in memoria"));

    // 3. ricerche
    let t = step("500 ricerche di fila");
    let mut slowest = Duration::ZERO;
    for (i, query) in [
        "sport", "news", "canale 1", "zz", "è", "\"", "a*", "%", "tg 24", "x",
    ]
    .iter()
    .cycle()
    .take(500)
    .enumerate()
    {
        let one = Instant::now();
        h.service.send(Command::Search {
            query: (*query).into(),
            generation: i as u64,
        });
        h.wait(30, |e| match e {
            Event::SearchResults { generation, .. } if generation == i as u64 => Some(()),
            _ => None,
        })?;
        slowest = slowest.max(one.elapsed());
    }
    done(t, &format!("la più lenta {} ms", slowest.as_millis()));

    // 4. raffica di comandi concorrenti mentre la playlist si riaggiorna
    let t = step("raffica: 5000 cronologia/preferiti + aggiornamento");
    h.service.send(Command::RefreshSource(source_id));
    for i in 0..5000 {
        let key = format!("k{}", i % 700);
        if i % 3 == 0 {
            h.service.send(Command::SetFavorite {
                kind: ItemKind::Live,
                key,
                favorite: i % 2 == 0,
            });
        } else {
            h.service.send(Command::SaveHistory {
                entry: HistoryEntry {
                    kind: Some(ItemKind::Movie),
                    key,
                    title: "Titolo di prova".into(),
                    position: f64::from(i),
                    duration: 6000.0,
                    watched_at: now + i64::from(i),
                    ..HistoryEntry::default()
                },
                notify: i % 50 == 0,
            });
        }
    }
    h.wait(300, |e| match e {
        Event::SourceSaved { result, .. } => Some(result),
        _ => None,
    })?
    .map_err(|e| format!("aggiornamento: {e}"))?;
    // il servizio risponde ancora, in ordine
    h.service.send(Command::Search {
        query: "fine".into(),
        generation: 99_999,
    });
    h.wait(120, |e| match e {
        Event::SearchResults {
            generation: 99_999, ..
        } => Some(()),
        _ => None,
    })?;
    done(t, "");

    // 5. catalogo Xtream grande
    let t = step("catalogo Xtream finto (20000 live, 30000 film, 2000 serie)");
    let server = MockServer::start(Catalog::large(20_000, 30_000, 2_000));
    h.service.send(Command::AddXtream {
        source: NewSource {
            name: "Xtream di prova".into(),
            kind: SourceKind::Xtream,
            location: server.address(),
            username: Some(USERNAME.into()),
            user_agent: None,
            epg_url: None,
        },
        password: PASSWORD.into(),
    });
    let report = h.wait(600, |e| match e {
        Event::SourceSaved { result, .. } => Some(result),
        _ => None,
    })?;
    let report = report.map_err(|e| format!("xtream: {e}"))?;
    done(
        t,
        &format!(
            "{} live, {} film, {} serie",
            report.channels, report.movies, report.series
        ),
    );

    // 6. file rotti: errori puliti, niente crolli
    let t = step("file rotti (vuoto, binario, riga enorme, gz troncato)");
    let broken = [
        ("vuoto.m3u", Vec::new()),
        (
            "binario.m3u",
            (0..200_000u32).map(|i| (i * 7919 % 251) as u8).collect(),
        ),
        (
            "riga.m3u",
            format!(
                "#EXTM3U\n#EXTINF:-1,{}\nhttp://x/1\n",
                "a".repeat(20_000_000)
            )
            .into_bytes(),
        ),
    ];
    for (name, bytes) in broken {
        let path = dir.join(name);
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        h.service.send(Command::VerifySource(NewSource {
            name: name.into(),
            kind: SourceKind::M3uFile,
            location: path.display().to_string(),
            username: None,
            user_agent: None,
            epg_url: None,
        }));
        let _ = h.wait(120, |e| match e {
            Event::Verified(result) => Some(result),
            _ => None,
        })?;
    }
    let truncated = dir.join("troncata.xml.gz");
    let full = std::fs::read(&guide).map_err(|e| e.to_string())?;
    std::fs::write(&truncated, &full[..full.len() / 3]).map_err(|e| e.to_string())?;
    h.service.send(Command::SetSourceEpg {
        source_id,
        epg_url: Some(truncated.display().to_string()),
    });
    let _ = h.wait(600, |e| match e {
        Event::Epg {
            state: EpgState::Done(result),
            ..
        } => Some(result),
        _ => None,
    })?;
    done(t, "nessun crollo");
    drop(h);

    let t = step("archivio dopo le prove");
    check_integrity(&dir.join("archivio.db"))?;
    done(t, "integrity_check ok");

    // 7. chiusure forzate durante le scritture
    let db = dir.join("archivio.db");
    let t = step("20 chiusure forzate durante le scritture");
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    for round in 0..20u64 {
        let mut child = Process::new(&exe)
            .args(["--scrittore", &db.display().to_string()])
            .spawn()
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(300 + (round * 137) % 900));
        let _ = child.kill();
        let _ = child.wait();
        check_integrity(&db).map_err(|e| format!("giro {round}: {e}"))?;
    }
    done(t, "integrity_check ok ogni volta");

    // 8. due processi insieme sullo stesso archivio
    let t = step("due processi che scrivono insieme per 15 s");
    let children: Vec<_> = (0..2)
        .map(|_| {
            Process::new(&exe)
                .args(["--scrittore", &db.display().to_string(), "15"])
                .spawn()
        })
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut failures = 0;
    for mut child in children {
        if !child.wait().map_err(|e| e.to_string())?.success() {
            failures += 1;
        }
    }
    check_integrity(&db)?;
    done(
        t,
        &format!("integrity_check ok, {failures} processi su 2 fermati da errori"),
    );
    Ok(())
}

/// Figlio: scritture di ogni tipo in un ciclo stretto.
fn writer(path: &Path, secs: u64) -> Res<()> {
    let mut store = Store::open(path, Cipher::new(&KEY)).map_err(|e| e.to_string())?;
    let source = store
        .active_source()
        .map_err(|e| e.to_string())?
        .ok_or("nessuna sorgente")?;
    let end = Instant::now() + Duration::from_secs(secs);
    let mut i: i64 = 0;
    while Instant::now() < end {
        i += 1;
        let key = format!("w{}", i % 300);
        store
            .save_history(
                source,
                &HistoryEntry {
                    kind: Some(ItemKind::Live),
                    key: key.clone(),
                    title: "x".into(),
                    watched_at: now() + i,
                    ..HistoryEntry::default()
                },
            )
            .map_err(|e| e.to_string())?;
        store
            .set_favorite(source, ItemKind::Live, &key, i % 2 == 0, now())
            .map_err(|e| e.to_string())?;
        store
            .set_setting("stress", &i.to_string())
            .map_err(|e| e.to_string())?;
        if i % 200 == 0 {
            let entries = sintonia_core::m3u::parse_str(&small_playlist(i)).entries;
            store
                .replace_channels(source, &entries, &[], now())
                .map_err(|e| e.to_string())?;
            store.set_active_source(source).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn small_playlist(seed: i64) -> String {
    let mut text = String::from("#EXTM3U\n");
    for n in 0..500 {
        text.push_str(&format!(
            "#EXTINF:-1 tvg-id=\"c{n}\" group-title=\"G{}\",Canale {n} {seed}\nhttp://127.0.0.1/{n}.ts\n",
            n % 7
        ));
    }
    text
}

fn check_integrity(path: &Path) -> Res<()> {
    let conn = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    let result: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if result == "ok" {
        Ok(())
    } else {
        Err(format!("archivio rovinato: {result}"))
    }
}

/// Scrive una playlist inventata, con righe che il parser deve saltare o sopportare.
/// Restituisce i canali che il parser deve leggere.
fn write_playlist(path: &Path, channels: usize) -> Res<usize> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    let mut text = String::from("\u{feff}#EXTM3U url-tvg=\"guida.xml.gz\"\r\n");
    let groups = [
        "Sport",
        "Notizie",
        "Film",
        "Bambini",
        "Musica",
        "Regionali",
        "",
        "Ünïcødé ✓",
    ];
    for n in 0..channels {
        let group = groups[n % groups.len()];
        match n % 25 {
            // senza attributi
            0 => text.push_str(&format!("#EXTINF:-1,Canale nudo {n}\n")),
            // nome lunghissimo e virgole
            1 => text.push_str(&format!(
                "#EXTINF:-1 tvg-id=\"c{n}\" group-title=\"{group}\",Canale, con, virgole {n} {}\n",
                "lungo ".repeat(40)
            )),
            // righe sporche prima dell'indirizzo
            2 => text.push_str(&format!(
                "#EXTINF:-1 tvg-id=\"c{n}\" group-title=\"{group}\",Canale {n}\n#EXTVLCOPT:http-user-agent=Prova\n\n   \n"
            )),
            _ => text.push_str(&format!(
                "#EXTINF:-1 tvg-id=\"c{n}\" tvg-name=\"Canale {n}\" tvg-logo=\"http://127.0.0.1:9/{n}.png\" group-title=\"{group}\",Canale {n}\r\n"
            )),
        }
        text.push_str(&format!("http://127.0.0.1:9/live/{n}.ts\n"));
        if n % 1000 == 0 {
            // spazzatura che va saltata
            text.push_str("questa riga non vuol dire niente\n#EXTINF:senza fine\n");
        }
        if text.len() > 1 << 20 {
            out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            text.clear();
        }
    }
    out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())?;
    Ok(channels)
}

/// Guida XMLTV compressa: `channels` canali (gli stessi `c<n>` della playlist), programmi da ieri
/// in avanti, con caratteri da sfuggire nei titoli.
fn write_guide(path: &Path, channels: usize, programmes: usize) -> Res<()> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut gz =
        flate2::write::GzEncoder::new(std::io::BufWriter::new(file), flate2::Compression::fast());
    let mut text = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<tv>\n");
    for n in 0..channels {
        text.push_str(&format!(
            "<channel id=\"c{n}\"><display-name>Canale {n}</display-name></channel>\n"
        ));
    }
    let per_channel = (programmes / channels.max(1)).max(1);
    let start = now() - 86_400;
    let len = (9 * 86_400 / per_channel as i64).max(60);
    let mut written = 0;
    'outer: for n in 0..channels {
        for k in 0..per_channel {
            if written >= programmes {
                break 'outer;
            }
            let from = start + k as i64 * len;
            text.push_str(&format!(
                "<programme start=\"{}\" stop=\"{}\" channel=\"c{n}\"><title>Programma {k} &amp; «altro» &lt;{n}&gt;</title><desc>Descrizione {}</desc></programme>\n",
                xmltv(from),
                xmltv(from + len),
                "parole ".repeat(10)
            ));
            written += 1;
            if text.len() > 1 << 20 {
                gz.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
                text.clear();
            }
        }
    }
    text.push_str("</tv>\n");
    gz.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    gz.finish().map_err(|e| e.to_string())?;
    Ok(())
}

fn xmltv(secs: i64) -> String {
    // giorni dal 1970 → data (algoritmo di Howard Hinnant)
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}{:02}{:02}{:02} +0000",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn file_mb(path: &Path) -> String {
    let bytes = std::fs::metadata(path).map_or(0, |m| m.len());
    format!("{:.1}", bytes as f64 / 1_048_576.0)
}
