//! Strumento di sviluppo: prova tutti i canali di una playlist M3U e dice quali partono.
//!
//! ```text
//! cargo run -p sintonia-player --example probe -- <playlist.m3u> [secondi] [in parallelo]
//! ```
//!
//! Stampa solo numero, nome ed esito: mai gli indirizzi (possono contenere credenziali).
//! L'audio è muto e senza finestra il video non si decodifica: conta l'avvio del flusso.

use std::fs::File;
use std::io::BufReader;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sintonia_core::m3u;
use sintonia_player::{EndReason, LoadOptions, Player, PlayerConfig, PlayerEvent};

#[derive(Debug)]
enum Outcome {
    Ok(Duration),
    Failed(String),
    Timeout,
}

fn probe(entry: &m3u::Entry, timeout: Duration) -> Outcome {
    let (tx, rx) = mpsc::channel();
    let player = match Player::new(&PlayerConfig::default(), move |e| {
        let _ = tx.send(e);
    }) {
        Ok(player) => player,
        Err(err) => return Outcome::Failed(err.to_string()),
    };
    let _ = player.set_option("mute", "yes");
    // per gli esperimenti: PROBE_OPTS="nome=valore;nome=valore"
    if let Ok(extra) = std::env::var("PROBE_OPTS") {
        for pair in extra.split(';').filter(|p| !p.is_empty()) {
            if let Some((name, value)) = pair.split_once('=') {
                if let Err(err) = player.set_option(name, value) {
                    eprintln!("opzione {name}: {err}");
                }
            }
        }
    }
    let options = LoadOptions {
        user_agent: entry.user_agent.clone(),
        referrer: entry.referrer.clone(),
    };
    let start = Instant::now();
    if let Err(err) = player.load_with(&entry.url, &options) {
        return Outcome::Failed(err.to_string());
    }
    loop {
        let Some(left) = timeout.checked_sub(start.elapsed()) else {
            return Outcome::Timeout;
        };
        match rx.recv_timeout(left) {
            Ok(PlayerEvent::Playing) => return Outcome::Ok(start.elapsed()),
            Ok(PlayerEvent::Ended(EndReason::Error(e))) => return Outcome::Failed(e),
            Ok(PlayerEvent::Ended(EndReason::Eof)) => {
                return Outcome::Failed("fine immediata".into())
            }
            Ok(_) => {}
            Err(_) => return Outcome::Timeout,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("uso: probe <playlist.m3u> [secondi] [in parallelo]")?;
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
    let parallel: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(6).max(1);
    // facoltativo: solo alcune voci, numeri da 1 separati da virgole
    let only: Option<Vec<usize>> = args
        .next()
        .map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect());
    let playlist = m3u::parse_reader(BufReader::new(File::open(path)?))?;
    let entries: Vec<m3u::Entry> = match &only {
        Some(only) => playlist
            .entries
            .into_iter()
            .enumerate()
            .filter(|(i, _)| only.contains(&(i + 1)))
            .map(|(_, e)| e)
            .collect(),
        None => playlist.entries,
    };
    let timeout = Duration::from_secs(secs);

    let mut results: Vec<Option<Outcome>> = (0..entries.len()).map(|_| None).collect();
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        let chunks: Vec<Vec<usize>> = (0..parallel)
            .map(|w| (w..entries.len()).step_by(parallel).collect())
            .collect();
        for chunk in chunks {
            let tx = tx.clone();
            let entries = &entries;
            scope.spawn(move || {
                for i in chunk {
                    eprintln!("inizio {:>3} {}", i + 1, entries[i].name);
                    let started = Instant::now();
                    let outcome = probe(&entries[i], timeout);
                    // `probe` include anche la chiusura di mpv: se questo tempo supera il limite,
                    // è la chiusura a bloccarsi
                    eprintln!(
                        "fine   {:>3} {:?} in {:.1}s",
                        i + 1,
                        outcome,
                        started.elapsed().as_secs_f32()
                    );
                    let _ = tx.send((i, outcome));
                }
            });
        }
        drop(tx);
        for (i, outcome) in rx {
            results[i] = Some(outcome);
        }
    });

    let (mut ok, mut bad) = (0, 0);
    for (i, (entry, outcome)) in entries.iter().zip(results).enumerate() {
        let tag = if entry.drm { " [DRM]" } else { "" };
        match outcome {
            Some(Outcome::Ok(t)) => {
                ok += 1;
                println!(
                    "{:>3}  OK    {:>5.1}s  {}{tag}",
                    i + 1,
                    t.as_secs_f32(),
                    entry.name
                );
            }
            Some(Outcome::Failed(e)) => {
                bad += 1;
                println!("{:>3}  NO           {}{tag}  ({e})", i + 1, entry.name);
            }
            Some(Outcome::Timeout) | None => {
                bad += 1;
                println!("{:>3}  TEMPO        {}{tag}", i + 1, entry.name);
            }
        }
    }
    println!("\nfunzionano {ok} su {}; non partono {bad}", ok + bad);
    Ok(())
}
