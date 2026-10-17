//! Toglie le credenziali dagli indirizzi prima che finiscano nei log.
//!
//! Gli URL IPTV contengono spesso utente e password: nel percorso (Xtream Codes:
//! `http://host/live/utente/password/123.ts`), nella query (`?username=…&password=…`) o
//! come `utente:password@host`. Qui non si prova a riconoscere i singoli schemi: di ogni URL
//! si tiene solo schema, host e porta, e il resto diventa `…`.

use std::borrow::Cow;

/// Schemi che consideriamo indirizzi di rete da mascherare.
const SCHEMES: &[&str] = &[
    "http://", "https://", "rtmp://", "rtmps://", "rtsp://", "rtp://", "udp://", "mms://",
    "mmsh://", "srt://", "ftp://",
];

/// Riduce un singolo URL a `schema://host[:porta]/…`, togliendo `utente:password@`.
///
/// Se la stringa non è un URL di rete conosciuto la restituisce invariata.
///
/// ```
/// use sintonia_core::redact::redact_url;
/// assert_eq!(redact_url("http://u:p@tv.example:8080/live/u/p/1.ts"), "http://tv.example:8080/…");
/// assert_eq!(redact_url("C:\\video.mkv"), "C:\\video.mkv");
/// ```
pub fn redact_url(url: &str) -> Cow<'_, str> {
    match find_url(url, 0) {
        Some((0, end)) if end == url.len() => Cow::Owned(shorten(url)),
        _ => Cow::Borrowed(url),
    }
}

/// Maschera tutti gli URL di rete che compaiono in un testo libero (per esempio un messaggio di mpv).
///
/// ```
/// use sintonia_core::redact::redact_text;
/// assert_eq!(
///     redact_text("Opening http://h/get.php?username=a&password=b done"),
///     "Opening http://h/… done"
/// );
/// ```
pub fn redact_text(text: &str) -> Cow<'_, str> {
    let mut out = String::new();
    let mut copied = 0;
    let mut from = 0;
    while let Some((start, end)) = find_url(text, from) {
        out.push_str(&text[copied..start]);
        out.push_str(&shorten(&text[start..end]));
        copied = end;
        from = end;
    }
    if copied == 0 {
        Cow::Borrowed(text)
    } else {
        out.push_str(&text[copied..]);
        Cow::Owned(out)
    }
}

/// Cerca il prossimo URL a partire da `from`; restituisce l'intervallo in byte.
fn find_url(text: &str, from: usize) -> Option<(usize, usize)> {
    let lower = text.get(from..)?.to_ascii_lowercase();
    let start = SCHEMES
        .iter()
        .filter_map(|scheme| {
            lower.match_indices(scheme).find_map(|(at, _)| {
                // lo schema deve iniziare una parola: "xhttp://" non conta
                let before = text[..from + at].chars().next_back();
                match before {
                    Some(c) if c.is_ascii_alphanumeric() => None,
                    _ => Some(at),
                }
            })
        })
        .min()?;
    let start = from + start;
    let end = text[start..]
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
        .map_or(text.len(), |len| start + len);
    // la punteggiatura in coda ("…/1.ts," oppure "(http://…)") appartiene alla frase, non all'URL
    let url = text[start..end].trim_end_matches([',', '.', ';', ':', ')', ']', '}']);
    Some((start, start + url.len()))
}

/// `schema://[credenziali@]host[:porta][/resto]` → `schema://host[:porta]/…`
fn shorten(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return "…".to_owned();
    };
    let (scheme, rest) = url.split_at(scheme_end + 3);
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    // le credenziali stanno prima dell'ultima '@' dell'autorità
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority_end == rest.len() && host.len() == authority.len() {
        url.to_owned()
    } else {
        format!("{scheme}{host}/…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xtream_path_credentials_are_removed() {
        assert_eq!(
            redact_url("http://server.tv:8080/live/mario/segreta/1234.ts"),
            "http://server.tv:8080/…"
        );
    }

    #[test]
    fn query_credentials_are_removed() {
        assert_eq!(
            redact_url("https://h.example/get.php?username=mario&password=segreta&type=m3u"),
            "https://h.example/…"
        );
    }

    #[test]
    fn userinfo_is_removed() {
        assert_eq!(
            redact_url("rtsp://mario:segreta@cam.local:554"),
            "rtsp://cam.local:554/…"
        );
        assert_eq!(redact_url("http://a@b@host/x"), "http://host/…");
    }

    #[test]
    fn host_only_url_is_kept() {
        assert_eq!(
            redact_url("http://host.example:80"),
            "http://host.example:80"
        );
    }

    #[test]
    fn non_urls_are_untouched() {
        assert!(matches!(
            redact_url("/home/mario/lista.m3u"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            redact_text("nessun indirizzo qui"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn text_with_several_urls() {
        let text =
            "[ffmpeg] HTTP error 403 for HTTP://a/u/p/1.ts, retry 'https://b:1/x?password=p'";
        assert_eq!(
            redact_text(text),
            "[ffmpeg] HTTP error 403 for HTTP://a/…, retry 'https://b:1/…'"
        );
    }

    #[test]
    fn scheme_inside_a_word_is_not_an_url() {
        assert_eq!(redact_text("xhttp://a/b"), "xhttp://a/b");
    }

    #[test]
    fn non_ascii_text_around_urls() {
        assert_eq!(
            redact_text("perché http://a/b/c è giù"),
            "perché http://a/… è giù"
        );
    }
}
