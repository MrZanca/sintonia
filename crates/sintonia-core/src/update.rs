//! Avviso di nuova versione: si chiede a GitHub l'ultima Release di Sintonia e la si confronta con
//! la versione in uso. Niente server nostro: GitHub vede solo che qualcuno ha chiesto la pagina.

use serde::Deserialize;

/// Indirizzo dell'ultima Release (API pubblica di GitHub, senza account).
pub const LATEST_RELEASE: &str = "https://api.github.com/repos/MrZanca/sintonia/releases/latest";
/// Le pagine che l'avviso può aprire: solo quelle delle Release di Sintonia.
const RELEASES_PAGE: &str = "https://github.com/MrZanca/sintonia/releases/";

/// Versione in uso.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Una versione nuova da proporre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// Versione senza la «v» («0.4.2»).
    pub version: String,
    /// Pagina della Release.
    pub url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// `X.Y.Z` (con o senza «v» davanti) in numeri; altro testo dopo la patch si ignora.
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.trim().trim_start_matches(['v', 'V']);
    let mut parts = version.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch_text = parts.next()?;
    let digits: String = patch_text
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let patch = digits.parse().ok()?;
    Some((major, minor, patch))
}

/// La versione `latest` è più nuova di `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

/// L'indirizzo è una pagina delle Release di Sintonia (l'unico che l'avviso può aprire).
pub fn is_release_page(url: &str) -> bool {
    url.starts_with(RELEASES_PAGE) && !url.contains(char::is_whitespace)
}

/// Dalla risposta di GitHub: la versione nuova, se è più recente di `current` e non è una
/// bozza, un'anteprima o la versione che l'utente ha deciso di saltare.
pub fn from_response(json: &str, current: &str, skipped: Option<&str>) -> Option<Update> {
    let release: Release = serde_json::from_str(json).ok()?;
    if release.draft || release.prerelease || !is_newer(&release.tag_name, current) {
        return None;
    }
    let version = release
        .tag_name
        .trim()
        .trim_start_matches(['v', 'V'])
        .to_owned();
    if skipped == Some(version.as_str()) || !is_release_page(&release.html_url) {
        return None;
    }
    Some(Update {
        version,
        url: release.html_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_numbers() {
        assert!(is_newer("v0.4.10", "0.4.9"));
        assert!(is_newer("1.0.0", "0.9.99"));
        assert!(!is_newer("v0.4.1", "0.4.1"));
        assert!(!is_newer("0.4.0", "0.4.1"));
        assert!(is_newer("0.5.0-beta", "0.4.1"));
        assert!(!is_newer("ultima", "0.4.1"));
        assert!(!is_newer("0.5", "0.4.1"));
    }

    #[test]
    fn response_to_update() {
        let json = r#"{"tag_name":"v0.4.2","html_url":"https://github.com/MrZanca/sintonia/releases/tag/v0.4.2","draft":false,"prerelease":false,"assets":[]}"#;
        let update = from_response(json, "0.4.1", None).expect("versione nuova");
        assert_eq!(update.version, "0.4.2");
        assert!(update.url.ends_with("/tag/v0.4.2"));
        assert_eq!(from_response(json, "0.4.2", None), None, "già aggiornata");
        assert_eq!(from_response(json, "0.4.1", Some("0.4.2")), None, "saltata");
        let elsewhere = json.replace("github.com/MrZanca/sintonia", "esempio.it");
        assert_eq!(
            from_response(&elsewhere, "0.4.1", None),
            None,
            "pagina estranea"
        );
        let pre = json.replace("\"prerelease\":false", "\"prerelease\":true");
        assert_eq!(from_response(&pre, "0.4.1", None), None);
        assert_eq!(from_response("non json", "0.4.1", None), None);
    }
}
