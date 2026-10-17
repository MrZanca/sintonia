//! Parser M3U tollerante, letto una riga alla volta.
//!
//! Le playlist IPTV reali sono spesso "sporche": manca `#EXTM3U`, ci sono BOM e `\r\n`, attributi
//! senza virgolette o scritti male (`tvgid`, `vg-id`), virgole dentro i nomi, opzioni VLC/Kodi per
//! il singolo canale, voci ripetute. Il parser non si ferma mai: quello che non capisce lo salta e
//! lo annota in [`Warning`].
//!
//! Si usa a righe ([`Parser::push_line`]) così una playlist da centinaia di MB si legge mentre
//! arriva dalla rete, senza tenerla tutta in memoria come testo.
//!
//! ```
//! use sintonia_core::m3u::parse_str;
//! let list = parse_str("#EXTM3U\n#EXTINF:-1 tvg-id=\"uno.it\" group-title=\"Rai\",Rai 1\nhttp://h/1.m3u8\n");
//! assert_eq!(list.entries[0].name, "Rai 1");
//! assert_eq!(list.entries[0].group.as_deref(), Some("Rai"));
//! ```

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::io::BufRead;

/// Un canale (o un contenuto) della playlist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// Nome mostrato (il testo dopo la virgola di `#EXTINF`).
    pub name: String,
    /// Indirizzo del flusso. Può contenere credenziali: non va mai scritto nei log.
    pub url: String,
    /// Identificativo per la guida (`tvg-id`).
    pub tvg_id: Option<String>,
    /// Nome per la guida (`tvg-name`).
    pub tvg_name: Option<String>,
    /// Indirizzo del logo (`tvg-logo`).
    pub logo: Option<String>,
    /// Categoria (`group-title` o `#EXTGRP`).
    pub group: Option<String>,
    /// Numero del canale indicato dalla playlist (`tvg-chno`).
    pub number: Option<u32>,
    /// User-agent richiesto da questo canale.
    pub user_agent: Option<String>,
    /// Referrer richiesto da questo canale.
    pub referrer: Option<String>,
    /// Canale radio (`radio="true"`).
    pub radio: bool,
    /// Il fornitore indica una protezione (licenza DRM nelle opzioni Kodi).
    pub drm: bool,
}

/// Qualcosa che il parser ha saltato o corretto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// Il file non inizia con `#EXTM3U`.
    MissingHeader,
    /// Un `#EXTINF` senza l'indirizzo che dovrebbe seguirlo.
    MissingUrl {
        /// Riga di `#EXTINF` (da 1).
        line: usize,
    },
    /// Un indirizzo non valido.
    InvalidUrl {
        /// Riga (da 1).
        line: usize,
    },
    /// Stesso nome e stesso indirizzo di una voce precedente: saltata.
    Duplicate {
        /// Riga (da 1).
        line: usize,
    },
}

/// Risultato completo della lettura.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Playlist {
    /// Canali nell'ordine della playlist.
    pub entries: Vec<Entry>,
    /// Indirizzi della guida dichiarati nell'intestazione (`url-tvg`, `x-tvg-url`).
    pub epg_urls: Vec<String>,
    /// Problemi incontrati.
    pub warnings: Vec<Warning>,
}

/// Parser a righe. Vedi la documentazione del modulo.
#[derive(Debug, Default)]
pub struct Parser {
    line: usize,
    header_seen: bool,
    /// La voce in costruzione (attributi di `#EXTINF` e opzioni) finché non arriva l'indirizzo.
    next: Entry,
    /// Riga dell'ultimo `#EXTINF` non ancora chiuso da un indirizzo.
    open_extinf: Option<usize>,
    /// Categoria di `#EXTGRP`, valida per la voce successiva.
    next_group: Option<String>,
    seen: HashSet<u64>,
    epg_urls: Vec<String>,
    warnings: Vec<Warning>,
}

impl Parser {
    /// Nuovo parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Legge una riga (con o senza `\n`/`\r\n`). Restituisce la voce completata da questa riga.
    pub fn push_line(&mut self, raw: &str) -> Option<Entry> {
        self.line += 1;
        let mut line = raw.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        if line.is_empty() {
            return None;
        }
        // anche playlist concatenate: ogni #EXTM3U può portare indirizzi della guida
        if let Some(rest) = strip_prefix_ci(line, "#EXTM3U") {
            self.header_seen = true;
            self.read_header(rest);
            return None;
        }
        if !self.header_seen {
            // la prima riga utile non è l'intestazione: lo si segnala una volta sola
            self.header_seen = true;
            self.warnings.push(Warning::MissingHeader);
        }

        if line.starts_with('#') {
            if let Some(rest) = strip_prefix_ci(line, "#EXTINF:") {
                if let Some(at) = self.open_extinf.replace(self.line) {
                    self.warnings.push(Warning::MissingUrl { line: at });
                    // le opzioni già lette (user-agent…) valgono per la nuova voce
                    self.next = Entry {
                        user_agent: self.next.user_agent.take(),
                        referrer: self.next.referrer.take(),
                        drm: self.next.drm,
                        ..Entry::default()
                    };
                }
                self.read_extinf(rest);
            } else if let Some(rest) = strip_prefix_ci(line, "#EXTGRP:") {
                self.next_group = non_empty(rest.trim());
            } else if let Some(rest) = strip_prefix_ci(line, "#EXTVLCOPT:") {
                self.read_option(rest);
            } else if let Some(rest) = strip_prefix_ci(line, "#KODIPROP:") {
                self.read_kodi_prop(rest);
            } else if let Some(rest) = strip_prefix_ci(line, "#EXTHTTP:") {
                self.read_http_json(rest);
            }
            return None;
        }

        // indirizzo, eventualmente con intestazioni in stile Kodi: url|User-Agent=x&Referer=y
        if let Some((url, headers)) = line.split_once('|') {
            if headers.contains('=') {
                self.read_pipe_headers(headers);
                line = url.trim();
            }
        }
        if !is_plausible_url(line) {
            self.warnings.push(Warning::InvalidUrl { line: self.line });
            return None;
        }

        let mut entry = std::mem::take(&mut self.next);
        self.open_extinf = None;
        entry.url = line.to_owned();
        if entry.group.is_none() {
            entry.group = self.next_group.take();
        } else {
            self.next_group = None;
        }
        if entry.name.is_empty() {
            entry.name = entry
                .tvg_name
                .clone()
                .unwrap_or_else(|| name_from_url(&entry.url));
        }

        let mut hasher = DefaultHasher::new();
        (&entry.name, &entry.url).hash(&mut hasher);
        if !self.seen.insert(hasher.finish()) {
            self.warnings.push(Warning::Duplicate { line: self.line });
            return None;
        }
        Some(entry)
    }

    /// Chiude la lettura e restituisce intestazione e avvisi (le voci le ha già date `push_line`).
    pub fn finish(mut self) -> Playlist {
        if let Some(line) = self.open_extinf.take() {
            self.warnings.push(Warning::MissingUrl { line });
        }
        Playlist {
            entries: Vec::new(),
            epg_urls: self.epg_urls,
            warnings: self.warnings,
        }
    }

    fn read_header(&mut self, rest: &str) {
        for (key, value) in Attributes::new(rest) {
            if matches!(key.as_str(), "url-tvg" | "x-tvg-url" | "tvg-url") {
                for url in value.split(',').map(str::trim).filter(|u| !u.is_empty()) {
                    if !self.epg_urls.iter().any(|known| known == url) {
                        self.epg_urls.push(url.to_owned());
                    }
                }
            }
        }
    }

    fn read_extinf(&mut self, rest: &str) {
        let (attrs, title) = match find_title_comma(rest) {
            Some(at) => (&rest[..at], rest[at + 1..].trim()),
            None => (rest, ""),
        };
        // la durata (-1, 0, 123.4) precede gli attributi
        let attrs = attrs
            .trim_start()
            .trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.'));
        let next = &mut self.next;
        for (key, value) in Attributes::new(attrs) {
            let value = value.trim();
            match key.as_str() {
                "tvg-id" | "tvgid" | "tvg_id" | "vg-id" => next.tvg_id = non_empty(value),
                "channel-id" if next.tvg_id.is_none() => next.tvg_id = non_empty(value),
                "tvg-name" | "tvg_name" => next.tvg_name = non_empty(value),
                "tvg-logo" | "logo" | "tvg_logo" => next.logo = non_empty(value),
                "group-title" | "group" | "group_title" => next.group = non_empty(value),
                "tvg-chno" | "channel-number" | "tvg-num" | "ch-number" => {
                    next.number = value.parse().ok().filter(|n| *n > 0);
                }
                "radio" => next.radio = value.eq_ignore_ascii_case("true"),
                "user-agent" | "http-user-agent" => next.user_agent = non_empty(value),
                "referer" | "referrer" | "http-referrer" | "http-referer" => {
                    next.referrer = non_empty(value);
                }
                _ => {}
            }
        }
        next.name = title.to_owned();
    }

    fn read_option(&mut self, rest: &str) {
        let Some((key, value)) = rest.split_once('=') else {
            return;
        };
        let value = value.trim().trim_matches('"');
        match key.trim().to_ascii_lowercase().as_str() {
            "http-user-agent" => self.next.user_agent = non_empty(value),
            "http-referrer" | "http-referer" => self.next.referrer = non_empty(value),
            _ => {}
        }
    }

    fn read_kodi_prop(&mut self, rest: &str) {
        let Some((key, value)) = rest.split_once('=') else {
            return;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "inputstream.adaptive.license_type" | "inputstream.adaptive.license_key" => {
                self.next.drm = true;
            }
            "inputstream.adaptive.stream_headers" | "inputstream.adaptive.manifest_headers" => {
                self.read_pipe_headers(value.trim());
            }
            _ => {}
        }
    }

    /// `#EXTHTTP:{"User-Agent":"…","Referer":"…"}`: basta un'estrazione semplice delle stringhe.
    fn read_http_json(&mut self, rest: &str) {
        if let Some(ua) = json_string_field(rest, "user-agent") {
            self.next.user_agent = non_empty(&ua);
        }
        if let Some(referrer) =
            json_string_field(rest, "referer").or_else(|| json_string_field(rest, "referrer"))
        {
            self.next.referrer = non_empty(&referrer);
        }
    }

    /// `User-Agent=x&Referer=y` (valori eventualmente codificati con %xx).
    fn read_pipe_headers(&mut self, headers: &str) {
        for pair in headers.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let value = percent_decode(value.trim());
            match key.trim().to_ascii_lowercase().as_str() {
                "user-agent" => self.next.user_agent = non_empty(&value),
                "referer" | "referrer" => self.next.referrer = non_empty(&value),
                _ => {}
            }
        }
    }
}

/// Legge una playlist intera da un testo.
pub fn parse_str(text: &str) -> Playlist {
    let mut parser = Parser::new();
    let entries: Vec<Entry> = text.lines().filter_map(|l| parser.push_line(l)).collect();
    Playlist {
        entries,
        ..parser.finish()
    }
}

/// Legge una playlist da un flusso di byte. Il testo non UTF-8 viene corretto, non rifiutato.
pub fn parse_reader<R: BufRead>(mut reader: R) -> std::io::Result<Playlist> {
    let mut parser = Parser::new();
    let mut entries = Vec::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        if let Some(entry) = parser.push_line(&String::from_utf8_lossy(&buf)) {
            entries.push(entry);
        }
    }
    Ok(Playlist {
        entries,
        ..parser.finish()
    })
}

/// Riassunto dei problemi, per l'interfaccia.
pub fn count_skipped(warnings: &[Warning]) -> usize {
    warnings
        .iter()
        .filter(|w| !matches!(w, Warning::MissingHeader))
        .count()
}

fn strip_prefix_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let head = line.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &line[prefix.len()..])
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Posizione della virgola che separa attributi e titolo: la prima fuori dalle virgolette.
/// Con virgolette sbilanciate si ripiega sull'ultima virgola della riga.
fn find_title_comma(text: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, ',') => return Some(i),
            _ => {}
        }
    }
    if quote.is_some() {
        text.rfind(',')
    } else {
        None
    }
}

fn is_plausible_url(line: &str) -> bool {
    if line.chars().any(char::is_control) {
        return false;
    }
    match line.split_once("://") {
        Some((scheme, rest)) => {
            !rest.is_empty()
                && !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        // percorsi locali: C:\video.ts, /home/…, \\server\share
        None => {
            line.starts_with('/')
                || line.starts_with("\\\\")
                || line.get(1..3) == Some(":\\")
                || line.get(1..3) == Some(":/")
        }
    }
}

/// Un nome leggibile per voci senza `#EXTINF`: l'ultimo pezzo del percorso, senza credenziali.
fn name_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path
        .trim_end_matches('/')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let stem = last.rsplit_once('.').map_or(last, |(stem, _)| stem);
    if stem.is_empty() || stem.contains(':') {
        "?".to_owned()
    } else {
        percent_decode(stem)
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Estrae `"chiave": "valore"` (chiave senza distinzione di maiuscole) da un piccolo oggetto JSON.
fn json_string_field(json: &str, key: &str) -> Option<String> {
    let lower = json.to_ascii_lowercase();
    let at = lower.find(&format!("\"{key}\""))?;
    let after = &json[at + key.len() + 2..];
    let after = after.trim_start().strip_prefix(':')?.trim_start();
    let after = after.strip_prefix('"')?;
    let mut value = String::new();
    let mut chars = after.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(value),
            '\\' => value.extend(chars.next()),
            c => value.push(c),
        }
    }
    None
}

/// Iteratore sugli attributi `chiave=valore` (valore tra "…", '…' o fino allo spazio).
/// Le chiavi escono in minuscolo.
struct Attributes<'a> {
    rest: &'a str,
}

impl<'a> Attributes<'a> {
    fn new(text: &'a str) -> Self {
        Self { rest: text }
    }
}

impl Iterator for Attributes<'_> {
    type Item = (String, String);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let rest = self.rest.trim_start();
            if rest.is_empty() {
                return None;
            }
            let key_end = rest
                .find(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')))
                .unwrap_or(rest.len());
            let key = &rest[..key_end];
            let after_key = rest[key_end..].trim_start();
            let Some(after_eq) = after_key.strip_prefix('=') else {
                // parola senza valore o carattere inatteso: si salta fino al prossimo spazio
                // (almeno un carattere, per avanzare sempre)
                let first = rest.chars().next().map_or(0, char::len_utf8);
                let skip = rest[first..]
                    .find(char::is_whitespace)
                    .map_or(rest.len(), |i| i + first);
                self.rest = &rest[skip..];
                continue;
            };
            let after_eq = after_eq.trim_start();
            let (value, remaining) = match after_eq.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let body = &after_eq[1..];
                    match body.find(q) {
                        Some(end) => (&body[..end], &body[end + 1..]),
                        None => (body, ""),
                    }
                }
                _ => {
                    let end = after_eq.find(char::is_whitespace).unwrap_or(after_eq.len());
                    (&after_eq[..end], &after_eq[end..])
                }
            };
            self.rest = remaining;
            if key.is_empty() {
                continue;
            }
            return Some((key.to_ascii_lowercase(), value.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> Entry {
        let list = parse_str(text);
        assert_eq!(list.entries.len(), 1, "voci: {:?}", list.entries);
        list.entries.into_iter().next().unwrap()
    }

    #[test]
    fn standard_entry() {
        let e = one(
            "#EXTM3U url-tvg=\"http://e/a.xml.gz,http://e/b.xml\"\n\
             #EXTINF:-1 tvg-id=\"rai1.it\" tvg-name=\"Rai 1\" tvg-logo=\"http://l/r1.png\" group-title=\"Rai\" tvg-chno=\"1\",Rai 1 HD\n\
             http://h/live/1.m3u8\n",
        );
        assert_eq!(e.name, "Rai 1 HD");
        assert_eq!(e.tvg_id.as_deref(), Some("rai1.it"));
        assert_eq!(e.tvg_name.as_deref(), Some("Rai 1"));
        assert_eq!(e.logo.as_deref(), Some("http://l/r1.png"));
        assert_eq!(e.group.as_deref(), Some("Rai"));
        assert_eq!(e.number, Some(1));
        assert_eq!(e.url, "http://h/live/1.m3u8");
    }

    #[test]
    fn header_epg_urls() {
        let list = parse_str("#EXTM3U x-tvg-url=\"http://e/a.xml , http://e/b.xml\"\n");
        assert_eq!(list.epg_urls, ["http://e/a.xml", "http://e/b.xml"]);
    }

    #[test]
    fn bom_crlf_and_missing_header() {
        let list = parse_str("\u{feff}#EXTINF:-1,Uno\r\nhttp://h/1\r\n");
        assert_eq!(list.entries[0].name, "Uno");
        assert!(list.warnings.contains(&Warning::MissingHeader));
        let list = parse_str("\u{feff}#EXTM3U\r\n#EXTINF:-1,Uno\r\nhttp://h/1\r\n");
        assert!(list.warnings.is_empty());
    }

    #[test]
    fn commas_inside_quotes_and_title() {
        let e = one("#EXTINF:-1 group-title=\"Film, Serie\",Canale, con virgola\nhttp://h/1\n");
        assert_eq!(e.group.as_deref(), Some("Film, Serie"));
        assert_eq!(e.name, "Canale, con virgola");
    }

    #[test]
    fn unquoted_and_misspelled_attributes() {
        let e = one("#EXTINF:0 tvgid=abc vg-id=def group-title=News tvg-logo=http://l/x.png,Nome\nhttp://h/1\n");
        assert_eq!(e.tvg_id.as_deref(), Some("def"));
        assert_eq!(e.group.as_deref(), Some("News"));
        assert_eq!(e.logo.as_deref(), Some("http://l/x.png"));
    }

    #[test]
    fn channel_id_is_a_fallback_for_tvg_id() {
        let e = one("#EXTINF:-1 channel-id=\"42\",A\nhttp://h/1\n");
        assert_eq!(e.tvg_id.as_deref(), Some("42"));
        let e = one("#EXTINF:-1 tvg-id=\"x\" channel-id=\"42\",A\nhttp://h/1\n");
        assert_eq!(e.tvg_id.as_deref(), Some("x"));
    }

    #[test]
    fn extgrp_and_vlc_options() {
        let e = one(
            "#EXTM3U\n#EXTGRP:Sport\n#EXTINF:-1,Uno\n#EXTVLCOPT:http-user-agent=Mozilla/5.0 (X)\n\
             #EXTVLCOPT:http-referrer=https://r.example/\nhttp://h/1\n",
        );
        assert_eq!(e.group.as_deref(), Some("Sport"));
        assert_eq!(e.user_agent.as_deref(), Some("Mozilla/5.0 (X)"));
        assert_eq!(e.referrer.as_deref(), Some("https://r.example/"));
    }

    #[test]
    fn options_before_extinf_are_kept() {
        let e = one("#EXTVLCOPT:http-user-agent=UA\n#EXTINF:-1,Uno\nhttp://h/1\n");
        assert_eq!(e.user_agent.as_deref(), Some("UA"));
    }

    #[test]
    fn options_do_not_leak_to_the_next_entry() {
        let list = parse_str(
            "#EXTINF:-1,A\n#EXTVLCOPT:http-user-agent=UA\nhttp://h/1\n#EXTINF:-1,B\nhttp://h/2\n",
        );
        assert_eq!(list.entries[1].user_agent, None);
    }

    #[test]
    fn pipe_headers() {
        let e = one("#EXTINF:-1,A\nhttp://h/1.m3u8|User-Agent=My%20UA&Referer=https://r/\n");
        assert_eq!(e.url, "http://h/1.m3u8");
        assert_eq!(e.user_agent.as_deref(), Some("My UA"));
        assert_eq!(e.referrer.as_deref(), Some("https://r/"));
    }

    #[test]
    fn kodi_drm_and_headers() {
        let e = one(
            "#EXTINF:-1,A\n#KODIPROP:inputstream.adaptive.license_type=clearkey\n\
             #KODIPROP:inputstream.adaptive.stream_headers=User-Agent=K\nhttps://h/a.mpd\n",
        );
        assert!(e.drm);
        assert_eq!(e.user_agent.as_deref(), Some("K"));
    }

    #[test]
    fn exthttp_json() {
        let e = one("#EXTINF:-1,A\n#EXTHTTP:{\"User-Agent\":\"J \\\"q\\\"\",\"Referer\":\"https://r/\"}\nhttp://h/1\n");
        assert_eq!(e.user_agent.as_deref(), Some("J \"q\""));
        assert_eq!(e.referrer.as_deref(), Some("https://r/"));
    }

    #[test]
    fn duplicates_are_dropped_but_backups_kept() {
        let list = parse_str(
            "#EXTINF:-1,A\nhttp://h/1\n#EXTINF:-1,A\nhttp://h/1\n#EXTINF:-1,A\nhttp://h/backup\n",
        );
        assert_eq!(list.entries.len(), 2);
        assert_eq!(
            list.warnings,
            [Warning::MissingHeader, Warning::Duplicate { line: 4 }]
        );
    }

    #[test]
    fn extinf_without_url() {
        let list = parse_str(
            "#EXTM3U\n#EXTINF:-1,Rotto\n#EXTINF:-1,Buono\nhttp://h/1\n#EXTINF:-1,Finale\n",
        );
        assert_eq!(list.entries.len(), 1);
        assert_eq!(list.entries[0].name, "Buono");
        assert_eq!(
            list.warnings,
            [
                Warning::MissingUrl { line: 2 },
                Warning::MissingUrl { line: 5 }
            ]
        );
    }

    #[test]
    fn bare_urls_get_a_name() {
        let list =
            parse_str("#EXTM3U\nhttp://u:p@h/live/canale%20uno.ts?token=x\nC:\\video\\film.mkv\n");
        assert_eq!(list.entries[0].name, "canale uno");
        assert_eq!(list.entries[1].name, "film");
    }

    #[test]
    fn garbage_lines_are_skipped() {
        let list = parse_str("#EXTM3U\n#EXTINF:-1,A\nnon un url\nhttp://h/1\n");
        assert_eq!(list.entries.len(), 1);
        assert_eq!(list.entries[0].name, "A");
        assert_eq!(list.warnings, [Warning::InvalidUrl { line: 3 }]);
    }

    #[test]
    fn unbalanced_quotes_fall_back_to_last_comma() {
        let e = one("#EXTINF:-1 tvg-name=\"Rotto,Titolo\nhttp://h/1\n");
        assert_eq!(e.name, "Titolo");
    }

    #[test]
    fn missing_title_uses_tvg_name() {
        let e = one("#EXTINF:-1 tvg-name=\"Dal tvg\",\nhttp://h/1\n");
        assert_eq!(e.name, "Dal tvg");
    }

    #[test]
    fn radio_flag_and_number() {
        let e = one("#EXTINF:-1 radio=\"true\" tvg-chno=\"0\",R\nhttp://h/1\n");
        assert!(e.radio);
        assert_eq!(e.number, None);
    }

    #[test]
    fn non_utf8_bytes_are_tolerated() {
        let bytes = b"#EXTM3U\n#EXTINF:-1,Caf\xe9\nhttp://h/1\n";
        let list = parse_reader(&bytes[..]).unwrap();
        assert!(list.entries[0].name.starts_with("Caf"));
    }

    #[test]
    fn large_playlist_is_fast() {
        let mut text = String::from("#EXTM3U\n");
        for i in 0..50_000 {
            text.push_str(&format!(
                "#EXTINF:-1 tvg-id=\"c{i}\" tvg-logo=\"http://l/{i}.png\" group-title=\"Gruppo {}\",Canale {i}\nhttp://h/live/u/p/{i}.ts\n",
                i % 200
            ));
        }
        let start = std::time::Instant::now();
        let list = parse_str(&text);
        assert_eq!(list.entries.len(), 50_000);
        // largo anche per le build di debug
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }
}
