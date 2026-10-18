//! Legge una guida XMLTV (anche `.gz`) e, se data, la abbina a una playlist M3U.
//!
//! ```text
//! cargo run --release -p sintonia-core --example leggi_guida -- guida.xml.gz [lista.m3u]
//! ```
//!
//! Stampa solo conteggi e tempi: niente nomi di canali né indirizzi.

use std::time::Instant;

use sintonia_core::epg::{self, Matcher, XmltvChannel};
use sintonia_core::m3u::Parser;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(guide) = args.next() else {
        eprintln!("uso: leggi_guida guida.xml[.gz] [lista.m3u]");
        std::process::exit(2);
    };
    let started = Instant::now();
    let file = match std::fs::File::open(&guide) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("guida non apribile: {err}");
            std::process::exit(1);
        }
    };
    let reader = match epg::open(file) {
        Ok(reader) => reader,
        Err(err) => {
            eprintln!("guida non leggibile: {err}");
            std::process::exit(1);
        }
    };
    let mut channels: Vec<XmltvChannel> = Vec::new();
    let mut with_programmes = std::collections::HashSet::new();
    let stats = epg::parse(
        reader,
        (i64::MIN, i64::MAX),
        &mut |c| channels.push(c),
        &mut |p| {
            with_programmes.insert(p.channel);
        },
    );
    let stats = match stats {
        Ok(stats) => stats,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
    println!(
        "guida: {} canali ({} con programmi), {} programmi, {} saltati{}, {:.2} s",
        stats.channels,
        with_programmes.len(),
        stats.programmes,
        stats.skipped,
        if stats.truncated {
            ", file troncato"
        } else {
            ""
        },
        started.elapsed().as_secs_f64()
    );

    let Some(list) = args.next() else { return };
    let text = match std::fs::read_to_string(&list) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("playlist non leggibile: {err}");
            std::process::exit(1);
        }
    };
    let mut parser = Parser::new();
    let entries: Vec<_> = text.lines().filter_map(|l| parser.push_line(l)).collect();
    // prima i canali che hanno programmi: tra due nomi uguali vince quello con la guida
    channels.sort_by_key(|c| !with_programmes.contains(&c.id));
    let matcher = Matcher::new(channels.iter().map(|c| (c.id.as_str(), c.names.as_slice())));
    let matched = entries
        .iter()
        .filter(|e| {
            matcher
                .find(e.tvg_id.as_deref(), &e.name)
                .is_some_and(|id| with_programmes.contains(id))
        })
        .count();
    println!(
        "playlist: {} canali, {} con la guida",
        entries.len(),
        matched
    );
}
