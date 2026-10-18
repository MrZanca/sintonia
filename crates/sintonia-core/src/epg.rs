//! Guida dei programmi XMLTV, letta a flusso.
//!
//! I file arrivano a centinaia di MB (spesso compressi `.gz`): non si tengono mai in memoria.
//! [`parse`] legge un elemento alla volta e consegna canali e programmi a chi chiama, che li
//! scrive nell'archivio a blocchi. Il lettore è tollerante: programmi senza titolo o con orari
//! illeggibili si saltano, un `stop` mancante si ricava dal programma successivo dello stesso
//! canale, e un file troncato tiene quello che si è letto fino all'errore.
//!
//! [`Matcher`] abbina i canali della playlist a quelli della guida: prima l'identificativo
//! (`tvg-id`) uguale, poi l'identificativo o il nome normalizzati con [`normalize_name`].

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// Lunghezza massima di una descrizione salvata (caratteri).
const MAX_DESCRIPTION: usize = 600;
/// Durata data a un programma senza `stop` e senza un successivo (secondi).
const DEFAULT_DURATION: i64 = 3600;

/// Errori di lettura della guida.
#[derive(Debug, thiserror::Error)]
pub enum EpgError {
    /// File illeggibile.
    #[error("file della guida: {0}")]
    Io(String),
    /// Il contenuto non è XML (o è rotto prima di qualsiasi programma).
    #[error("guida non valida: {0}")]
    Xml(String),
}

/// Un canale dichiarato nella guida.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XmltvChannel {
    /// Identificativo (`id`), quello a cui puntano i programmi.
    pub id: String,
    /// Nomi (`display-name`), anche più di uno.
    pub names: Vec<String>,
}

/// Un programma.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Programme {
    /// Identificativo del canale nella guida.
    pub channel: String,
    /// Inizio (secondi Unix).
    pub start: i64,
    /// Fine (secondi Unix).
    pub stop: i64,
    /// Titolo.
    pub title: String,
    /// Sottotitolo (titolo dell'episodio…).
    pub subtitle: Option<String>,
    /// Descrizione (accorciata).
    pub description: Option<String>,
    /// Prima categoria.
    pub category: Option<String>,
    /// Numero dell'episodio come scritto nella guida.
    pub episode: Option<String>,
}

/// Quanto si è letto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParseStats {
    /// Canali.
    pub channels: usize,
    /// Programmi consegnati.
    pub programmes: usize,
    /// Programmi saltati (senza titolo, orari illeggibili, fuori dalla finestra).
    pub skipped: usize,
    /// Il file si è interrotto con un errore dopo aver dato qualcosa.
    pub truncated: bool,
}

/// Apre un flusso che può essere compresso con gzip (lo riconosce dai primi due byte).
pub fn open(input: impl Read + 'static) -> std::io::Result<Box<dyn BufRead>> {
    let mut reader = BufReader::with_capacity(64 * 1024, input);
    let gzip = reader.fill_buf()?.starts_with(&[0x1f, 0x8b]);
    Ok(if gzip {
        Box::new(BufReader::with_capacity(
            64 * 1024,
            flate2::bufread::MultiGzDecoder::new(reader),
        ))
    } else {
        Box::new(reader)
    })
}

/// Campo di testo che si sta leggendo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    DisplayName,
    Title,
    SubTitle,
    Desc,
    Category,
    Episode,
}

/// Legge una guida XMLTV. Si tengono solo i programmi che si sovrappongono a `window`
/// (secondi Unix, inizio incluso e fine esclusa).
pub fn parse(
    input: impl BufRead,
    window: (i64, i64),
    on_channel: &mut dyn FnMut(XmltvChannel),
    on_programme: &mut dyn FnMut(Programme),
) -> Result<ParseStats, EpgError> {
    let mut reader = Reader::from_reader(input);
    // niente trim: i riferimenti (&amp;) spezzano il testo e gli spazi attorno contano
    reader.config_mut().check_end_names = false;
    let mut stats = ParseStats::default();
    let mut buf = Vec::new();
    // canale o programma in lettura, profondità dentro l'elemento, campo e testo in lettura
    let mut channel: Option<XmltvChannel> = None;
    let mut programme: Option<Programme> = None;
    let mut depth = 0usize;
    let mut field: Option<Field> = None;
    let mut text = String::new();
    // programmi senza `stop`, in attesa del successivo dello stesso canale
    let mut open: HashMap<String, Programme> = HashMap::new();

    let mut deliver = |p: Programme, stats: &mut ParseStats| {
        if p.stop > window.0 && p.start < window.1 && p.stop > p.start && !p.title.is_empty() {
            stats.programmes += 1;
            on_programme(p);
        } else {
            stats.skipped += 1;
        }
    };

    loop {
        let event = match reader.read_event_into(&mut buf) {
            Ok(event) => event,
            Err(err) => {
                if stats.programmes == 0 && stats.channels == 0 {
                    return Err(EpgError::Xml(err.to_string()));
                }
                log::warn!("guida interrotta: {err}");
                stats.truncated = true;
                break;
            }
        };
        match event {
            Event::Start(e) => {
                let name = e.local_name();
                match (name.as_ref(), &channel, &programme) {
                    ("channel", None, None) => {
                        channel = Some(XmltvChannel {
                            id: attribute(&e, "id").unwrap_or_default(),
                            names: Vec::new(),
                        });
                        depth = 0;
                    }
                    ("programme", None, None) => {
                        programme = Some(start_programme(&e, &mut stats));
                        depth = 0;
                    }
                    (tag, _, _) if channel.is_some() || programme.is_some() => {
                        depth += 1;
                        if depth == 1 {
                            field = match (tag, channel.is_some()) {
                                ("display-name", true) => Some(Field::DisplayName),
                                ("title", false) => Some(Field::Title),
                                ("sub-title", false) => Some(Field::SubTitle),
                                ("desc", false) => Some(Field::Desc),
                                ("category", false) => Some(Field::Category),
                                ("episode-num", false) => Some(Field::Episode),
                                _ => None,
                            };
                            text.clear();
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                // <channel id="x"/>: un canale senza nomi
                if e.local_name().as_ref() == "channel" && channel.is_none() && programme.is_none()
                {
                    if let Some(id) = attribute(&e, "id") {
                        stats.channels += 1;
                        on_channel(XmltvChannel {
                            id,
                            names: Vec::new(),
                        });
                    }
                }
            }
            Event::Text(t) => {
                if field.is_some() {
                    text.push_str(&t.xml10_content());
                }
            }
            Event::CData(t) => {
                if field.is_some() {
                    text.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if field.is_some() {
                    match r.resolve_char_ref() {
                        Ok(Some(c)) => text.push(c),
                        _ => text.push_str(match &*r {
                            "amp" => "&",
                            "lt" => "<",
                            "gt" => ">",
                            "quot" => "\"",
                            "apos" => "'",
                            _ => "",
                        }),
                    }
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                if depth > 0 {
                    if depth == 1 {
                        if let Some(f) = field.take() {
                            let value = text.trim();
                            if let Some(c) = channel.as_mut() {
                                if f == Field::DisplayName && !value.is_empty() {
                                    c.names.push(value.to_owned());
                                }
                            } else if let Some(p) = programme.as_mut() {
                                store_field(p, f, value);
                            }
                        }
                    }
                    depth -= 1;
                } else if name.as_ref() == "channel" {
                    if let Some(c) = channel.take() {
                        if !c.id.is_empty() {
                            stats.channels += 1;
                            on_channel(c);
                        }
                    }
                } else if name.as_ref() == "programme" {
                    if let Some(mut p) = programme.take() {
                        if p.start == i64::MIN || p.channel.is_empty() {
                            stats.skipped += 1;
                            continue;
                        }
                        // il programma precedente senza fine termina dove inizia questo
                        if let Some(mut previous) = open.remove(&p.channel) {
                            previous.stop = p.start;
                            deliver(previous, &mut stats);
                        }
                        if p.stop == i64::MIN {
                            p.stop = p.start;
                            open.insert(p.channel.clone(), p);
                        } else {
                            deliver(p, &mut stats);
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    for (_, mut p) in open {
        p.stop = p.start + DEFAULT_DURATION;
        deliver(p, &mut stats);
    }
    Ok(stats)
}

/// Inizio di un `<programme>`: orari e canale dagli attributi (`i64::MIN` se illeggibili).
fn start_programme(e: &BytesStart<'_>, _stats: &mut ParseStats) -> Programme {
    Programme {
        channel: attribute(e, "channel").unwrap_or_default(),
        start: attribute(e, "start")
            .and_then(|t| parse_time(&t))
            .unwrap_or(i64::MIN),
        stop: attribute(e, "stop")
            .and_then(|t| parse_time(&t))
            .unwrap_or(i64::MIN),
        ..Programme::default()
    }
}

fn store_field(p: &mut Programme, field: Field, value: &str) {
    if value.is_empty() {
        return;
    }
    // di ogni campo vale il primo (le guide ripetono il titolo in più lingue)
    let slot = match field {
        Field::Title => {
            if p.title.is_empty() {
                p.title = value.to_owned();
            }
            return;
        }
        Field::SubTitle => &mut p.subtitle,
        Field::Desc => &mut p.description,
        Field::Category => &mut p.category,
        Field::Episode => &mut p.episode,
        Field::DisplayName => return,
    };
    if slot.is_none() {
        *slot = Some(if field == Field::Desc {
            truncate(value, MAX_DESCRIPTION)
        } else {
            value.to_owned()
        });
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", text[..end].trim_end()),
        None => text.to_owned(),
    }
}

fn attribute(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.trim().to_owned())
        })
        .filter(|v| !v.is_empty())
}

/// Orario XMLTV (`20261017203000 +0200`, anche senza secondi, senza ora o senza fuso: allora è
/// UTC) in secondi Unix.
pub fn parse_time(text: &str) -> Option<i64> {
    let text = text.trim();
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    let rest = text[digits.len()..].trim();
    let field = |range: std::ops::Range<usize>| -> Option<i64> {
        digits.get(range).map_or(Some(0), |s| s.parse().ok())
    };
    if digits.len() < 8 {
        return None;
    }
    let (year, month, day) = (field(0..4)?, field(4..6)?, field(6..8)?);
    let (hour, minute, second) = (field(8..10)?, field(10..12)?, field(12..14)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let offset = parse_offset(rest)?;
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second.min(59) - offset)
}

/// Fuso `+hhmm`/`-hhmm` (o `+hh:mm`) in secondi; vuoto o `Z`/`UTC`/`GMT` è zero.
fn parse_offset(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() || matches!(text, "Z" | "UTC" | "GMT") {
        return Some(0);
    }
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return Some(0),
    };
    let digits: String = digits.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 4 {
        return None;
    }
    let hours: i64 = digits[0..2].parse().ok()?;
    let minutes: i64 = digits[2..4].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Giorni dal 1970-01-01 (algoritmo di Howard Hinnant, calendario gregoriano).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parole che non distinguono un canale da un altro.
const NOISE: [&str; 9] = [
    "hd", "fhd", "uhd", "sd", "4k", "hevc", "h265", "backup", "bk",
];

/// Nome o identificativo ridotto alla sua forma essenziale, per abbinare canali scritti in
/// modo diverso: «Tele 1 HD», «Tele.1.HD..101.it» e «Tele1.it» diventano tutti «tele1».
///
/// Si tolgono il suffisso del paese (`.it`), accenti, maiuscole, punteggiatura, le parole di
/// qualità (HD, 4K…) e i numeri lunghi (numerazioni del fornitore); il `+` resta (Tele Sport +).
pub fn normalize_name(text: &str) -> String {
    let mut text = text.trim();
    if let Some((head, tail)) = text.rsplit_once('.') {
        if tail.len() == 2 && tail.chars().all(|c| c.is_ascii_alphabetic()) && !head.is_empty() {
            text = head;
        }
    }
    let folded: String = text.chars().map(fold_char).collect();
    folded
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '+'))
        .filter(|t| !t.is_empty())
        .filter(|t| !NOISE.contains(t))
        .filter(|t| !(t.len() >= 3 && t.chars().all(|c| c.is_ascii_digit())))
        .collect()
}

/// Minuscola senza accenti (lettere latine comuni); altri caratteri diventano separatori.
fn fold_char(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        c if c.is_ascii() => c,
        _ => ' ',
    }
}

/// Abbina i canali della playlist ai canali della guida.
#[derive(Debug, Default)]
pub struct Matcher {
    exact: HashMap<String, String>,
    normalized: HashMap<String, String>,
}

impl Matcher {
    /// Indici dei canali della guida.
    pub fn new<'a>(channels: impl IntoIterator<Item = (&'a str, &'a [String])>) -> Self {
        let mut matcher = Self::default();
        for (id, names) in channels {
            matcher
                .exact
                .entry(id.to_lowercase())
                .or_insert_with(|| id.to_owned());
            for name in std::iter::once(id).chain(names.iter().map(String::as_str)) {
                let key = normalize_name(name);
                if !key.is_empty() {
                    matcher
                        .normalized
                        .entry(key)
                        .or_insert_with(|| id.to_owned());
                }
            }
        }
        matcher
    }

    /// Identificativo nella guida del canale con questo `tvg-id` e questo nome.
    pub fn find(&self, tvg_id: Option<&str>, name: &str) -> Option<&str> {
        let tvg_id = tvg_id.map(str::trim).filter(|t| !t.is_empty());
        tvg_id
            .and_then(|t| self.exact.get(&t.to_lowercase()))
            .or_else(|| tvg_id.and_then(|t| self.normalized.get(&normalize_name(t))))
            .or_else(|| self.normalized.get(&normalize_name(name)))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE tv SYSTEM "xmltv.dtd">
<tv generator-info-name="prova">
  <channel id="Uno.it"><display-name lang="it">Canale Uno HD</display-name><icon src="x"/></channel>
  <channel id="Due.it"/>
  <programme start="20261017200000 +0200" stop="20261017203000 +0200" channel="Uno.it">
    <title lang="it">Tg &amp; meteo</title>
    <title lang="en">News</title>
    <sub-title>Edizione &#232; serale</sub-title>
    <desc><![CDATA[Le notizie <della> sera]]></desc>
    <category>News</category><category>Info</category>
    <credits><actor>Mario</actor></credits>
    <episode-num system="onscreen">S1E2</episode-num>
  </programme>
  <programme start="20261017203000 +0200" channel="Uno.it"><title>Film</title></programme>
  <programme start="20261017220000 +0200" stop="20261017230000 +0200" channel="Uno.it"><title>Notte</title></programme>
  <programme start="nonsense" stop="20261017230000" channel="Uno.it"><title>Rotto</title></programme>
  <programme start="20261017200000" stop="20261017210000" channel="Due.it"><title></title></programme>
  <programme start="20261020200000" channel="Due.it"><title>Ultimo</title></programme>
</tv>"#;

    fn read(xml: &str, window: (i64, i64)) -> (Vec<XmltvChannel>, Vec<Programme>, ParseStats) {
        let (mut channels, mut programmes) = (Vec::new(), Vec::new());
        let stats = parse(
            xml.as_bytes(),
            window,
            &mut |c| channels.push(c),
            &mut |p| programmes.push(p),
        )
        .expect("guida leggibile");
        (channels, programmes, stats)
    }

    #[test]
    fn reads_channels_and_programmes() {
        let (channels, programmes, stats) = read(SAMPLE, (0, i64::MAX));
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].names, ["Canale Uno HD"]);
        assert_eq!(channels[1].id, "Due.it");
        let first = &programmes[0];
        assert_eq!(first.title, "Tg & meteo", "primo titolo, entità risolte");
        assert_eq!(first.subtitle.as_deref(), Some("Edizione è serale"));
        assert_eq!(
            first.description.as_deref(),
            Some("Le notizie <della> sera")
        );
        assert_eq!(first.category.as_deref(), Some("News"));
        assert_eq!(first.episode.as_deref(), Some("S1E2"));
        // 20:00 +0200 = 18:00 UTC
        assert_eq!(first.start, parse_time("20261017180000").unwrap_or(0));
        assert_eq!(first.stop - first.start, 1800);
        // senza stop: finisce dove inizia il successivo
        let film = programmes.iter().find(|p| p.title == "Film").expect("film");
        assert_eq!(film.stop - film.start, 90 * 60);
        // l'ultimo senza stop dura un'ora
        let last = programmes
            .iter()
            .find(|p| p.title == "Ultimo")
            .expect("ultimo");
        assert_eq!(last.stop - last.start, DEFAULT_DURATION);
        assert_eq!(programmes.len(), 4);
        assert_eq!(stats.skipped, 2, "orario illeggibile e titolo vuoto");
        assert!(!stats.truncated);
    }

    #[test]
    fn window_filters_programmes() {
        let from = parse_time("20261017190000 +0200").unwrap_or(0);
        let to = parse_time("20261017210000 +0200").unwrap_or(0);
        let (_, programmes, _) = read(SAMPLE, (from, to));
        let titles: Vec<&str> = programmes.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Tg & meteo", "Film"]);
    }

    #[test]
    fn truncated_file_keeps_what_was_read() {
        let cut = &SAMPLE[..SAMPLE
            .find("<programme start=\"20261017220000")
            .unwrap_or(0)
            + 40];
        let (_, programmes, stats) = read(cut, (0, i64::MAX));
        assert!(stats.truncated || !programmes.is_empty());
        assert!(programmes.iter().any(|p| p.title == "Tg & meteo"));
        assert!(parse("non è xml <<<".as_bytes(), (0, 1), &mut |_| {}, &mut |_| {}).is_err());
    }

    #[test]
    fn gzip_is_detected() {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(SAMPLE.as_bytes()).expect("compressione");
        let gz = encoder.finish().expect("compressione");
        let reader = open(std::io::Cursor::new(gz)).expect("apertura");
        let mut count = 0;
        parse(reader, (0, i64::MAX), &mut |_| {}, &mut |_| count += 1).expect("lettura");
        assert_eq!(count, 4);
        let plain = open(std::io::Cursor::new(SAMPLE.as_bytes().to_vec())).expect("apertura");
        let mut count = 0;
        parse(plain, (0, i64::MAX), &mut |_| {}, &mut |_| count += 1).expect("lettura");
        assert_eq!(count, 4);
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("19700101000000 +0000"), Some(0));
        assert_eq!(parse_time("19700101010000 +0100"), Some(0));
        assert_eq!(parse_time("19700101000000 -0130"), Some(5400));
        assert_eq!(parse_time("197001020000"), Some(86_400), "senza secondi");
        assert_eq!(parse_time("20000301"), Some(951_868_800), "solo data");
        assert_eq!(
            parse_time("20240229120000 Z"),
            Some(1_709_208_000),
            "anno bisestile"
        );
        assert_eq!(parse_time("20261025010000 +01:00"), Some(1_792_886_400));
        assert_eq!(parse_time("2026"), None);
        assert_eq!(parse_time("20261340000000"), None);
    }

    #[test]
    fn names_are_normalized() {
        assert_eq!(normalize_name("Tele.1.HD..101.it"), "tele1");
        assert_eq!(normalize_name("Tele1.it"), "tele1");
        assert_eq!(normalize_name("TELE 1 FHD"), "tele1");
        assert_eq!(normalize_name("Tele Sport + HD"), "telesport+");
        assert_eq!(normalize_name("Canale Più"), "canalepiu");
        assert_eq!(normalize_name("TV9"), "tv9");
        assert_eq!(normalize_name("Tele.4..5021.it"), "tele4");
        assert_eq!(normalize_name("20"), "20", "numeri brevi restano");
    }

    #[test]
    fn matcher_prefers_ids_then_names() {
        let channels = [
            ("Tele.1.HD..101.it".to_owned(), vec!["Tele 1 HD".to_owned()]),
            ("Rete.5.it".to_owned(), vec!["Rete 5".to_owned()]),
            ("Exact.it".to_owned(), vec![]),
        ];
        let matcher = Matcher::new(channels.iter().map(|(id, n)| (id.as_str(), n.as_slice())));
        assert_eq!(matcher.find(Some("exact.IT"), "altro"), Some("Exact.it"));
        assert_eq!(matcher.find(Some("Tele1.it"), "x"), Some("Tele.1.HD..101.it"));
        assert_eq!(matcher.find(None, "Rete 5 HD"), Some("Rete.5.it"));
        assert_eq!(matcher.find(Some(""), "Tele 1"), Some("Tele.1.HD..101.it"));
        assert_eq!(matcher.find(Some("Nessuno.it"), "Nessuno"), None);
    }
}
