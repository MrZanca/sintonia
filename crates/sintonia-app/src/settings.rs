//! Impostazioni dell'utente: chiavi e valori salvati nell'archivio (tabella `settings`).

use std::collections::{HashMap, HashSet};

use sintonia_core::db::ItemKind;
use sintonia_core::service::{EPG_HOURS_DEFAULT, PLAYLIST_HOURS_DEFAULT};

/// Lingua dell'interfaccia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Italian,
    English,
}

impl Language {
    /// Codice della cartella delle traduzioni (`"en"` sono le stringhe originali).
    pub fn code(self) -> &'static str {
        match self {
            Self::Italian => "it",
            Self::English => "en",
        }
    }

    /// Posizione nell'elenco delle Impostazioni.
    pub fn index(self) -> i32 {
        match self {
            Self::Italian => 0,
            Self::English => 1,
        }
    }

    pub fn from_index(index: i32) -> Self {
        if index == 1 {
            Self::English
        } else {
            Self::Italian
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code.get(..2)?.to_ascii_lowercase().as_str() {
            "it" => Some(Self::Italian),
            "en" => Some(Self::English),
            _ => None,
        }
    }

    /// Lingua del sistema: italiano se il sistema è in italiano, altrimenti inglese.
    pub fn system() -> Self {
        sys_locale::get_locale()
            .and_then(|l| Self::from_code(&l))
            .unwrap_or(Self::English)
    }
}

/// Le impostazioni in memoria.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub language: Language,
    pub reconnect: bool,
    pub fullscreen_start: bool,
    /// Nell'elenco Live il canale evidenziato si apre da solo dopo un attimo.
    pub live_preview: bool,
    /// Ogni quante ore playlist e guida si aggiornano da sole (0 = mai; li legge il servizio).
    pub playlist_hours: u32,
    pub epg_hours: u32,
    /// User-agent generale (vuoto = quello di Sintonia).
    pub user_agent: Option<String>,
    /// Chiave stabile dell'ultimo canale visto.
    pub last_channel: Option<String>,
    /// Larghezza del mini player (pixel logici).
    pub pip_width: Option<f32>,
    /// Impronta del PIN del controllo genitori (vedi `sintonia_core::pin`).
    pub pin: Option<String>,
    /// Categorie bloccate dal PIN, per sorgente.
    pub locks: HashMap<i64, Locks>,
}

/// Le categorie bloccate di una sorgente (nomi come arrivano dal fornitore).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Locks {
    pub live: HashSet<String>,
    pub movies: HashSet<String>,
    pub series: HashSet<String>,
}

impl Locks {
    /// Chiave dell'impostazione per la sorgente `source_id`.
    pub fn key(source_id: i64) -> String {
        format!("locks.{source_id}")
    }

    /// Blocca o sblocca una categoria.
    pub fn set(&mut self, kind: ItemKind, group: &str, locked: bool) {
        let set = match kind {
            ItemKind::Live => &mut self.live,
            ItemKind::Movie => &mut self.movies,
            ItemKind::Series => &mut self.series,
        };
        if locked {
            set.insert(group.to_owned());
        } else {
            set.remove(group);
        }
    }

    /// Una riga per categoria: `l`, `m` o `s`, un tab, il nome (i nomi non hanno a capo).
    pub fn to_value(&self) -> String {
        let mut lines: Vec<String> = [("l", &self.live), ("m", &self.movies), ("s", &self.series)]
            .into_iter()
            .flat_map(|(tag, set)| set.iter().map(move |g| format!("{tag}\t{g}")))
            .collect();
        lines.sort();
        lines.join("\n")
    }

    pub fn from_value(value: &str) -> Self {
        let mut locks = Self::default();
        for line in value.lines() {
            let Some((tag, group)) = line.split_once('\t') else {
                continue;
            };
            let kind = match tag {
                "l" => ItemKind::Live,
                "m" => ItemKind::Movie,
                "s" => ItemKind::Series,
                _ => continue,
            };
            locks.set(kind, group, true);
        }
        locks
    }
}

impl Settings {
    pub fn defaults(language: Language) -> Self {
        Self {
            language,
            reconnect: true,
            fullscreen_start: false,
            live_preview: true,
            playlist_hours: PLAYLIST_HOURS_DEFAULT,
            epg_hours: EPG_HOURS_DEFAULT,
            user_agent: None,
            last_channel: None,
            pip_width: None,
            pin: None,
            locks: HashMap::new(),
        }
    }

    /// Dalle coppie salvate; le chiavi sconosciute si ignorano.
    pub fn from_pairs(pairs: &[(String, String)], default_language: Language) -> Self {
        let mut settings = Self::defaults(default_language);
        // prima c'era solo l'interruttore «Aggiorna la guida all'avvio»
        if pairs.iter().any(|(k, v)| k == "epg_on_start" && v == "0") {
            settings.epg_hours = 0;
        }
        for (key, value) in pairs {
            settings.set(key, value);
        }
        settings
    }

    /// Il blocco del PIN è attivo (c'è un PIN).
    pub fn has_pin(&self) -> bool {
        self.pin.is_some()
    }

    /// Aggiorna una chiave.
    pub fn set(&mut self, key: &str, value: &str) {
        let flag = value == "1";
        match key {
            "language" => {
                if let Some(language) = Language::from_code(value) {
                    self.language = language;
                }
            }
            "reconnect" => self.reconnect = flag,
            "fullscreen_start" => self.fullscreen_start = flag,
            "live_preview" => self.live_preview = flag,
            "playlist_hours" => {
                if let Ok(hours) = value.trim().parse() {
                    self.playlist_hours = hours;
                }
            }
            "epg_hours" => {
                if let Ok(hours) = value.trim().parse() {
                    self.epg_hours = hours;
                }
            }
            "pin" => self.pin = Some(value.to_owned()).filter(|v| !v.is_empty()),
            "user_agent" => {
                self.user_agent = Some(value.trim())
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned);
            }
            "last_channel" => self.last_channel = Some(value.to_owned()),
            "pip_width" => self.pip_width = value.parse().ok().filter(|w: &f32| w.is_finite()),
            _ => {
                if let Some(id) = key.strip_prefix("locks.").and_then(|id| id.parse().ok()) {
                    self.locks.insert(id, Locks::from_value(value));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_values_override_defaults() {
        let pairs = [
            ("language".to_owned(), "en".to_owned()),
            ("reconnect".to_owned(), "0".to_owned()),
            ("live_preview".to_owned(), "0".to_owned()),
            ("user_agent".to_owned(), "  ".to_owned()),
            ("sconosciuta".to_owned(), "x".to_owned()),
        ];
        let s = Settings::from_pairs(&pairs, Language::Italian);
        assert_eq!(s.language, Language::English);
        assert!(!s.reconnect);
        assert!(!s.live_preview);
        assert_eq!(s.user_agent, None);
        assert_eq!(s.playlist_hours, PLAYLIST_HOURS_DEFAULT);
        assert!(!s.has_pin());
    }

    #[test]
    fn old_epg_switch_and_intervals() {
        let off = [("epg_on_start".to_owned(), "0".to_owned())];
        assert_eq!(Settings::from_pairs(&off, Language::Italian).epg_hours, 0);
        let pairs = [
            ("epg_on_start".to_owned(), "0".to_owned()),
            ("epg_hours".to_owned(), "24".to_owned()),
            ("playlist_hours".to_owned(), "0".to_owned()),
        ];
        let s = Settings::from_pairs(&pairs, Language::Italian);
        assert_eq!((s.epg_hours, s.playlist_hours), (24, 0));
    }

    #[test]
    fn locks_round_trip() {
        let mut locks = Locks::default();
        locks.set(ItemKind::Live, "Adulti", true);
        locks.set(ItemKind::Movie, "Horror", true);
        locks.set(ItemKind::Series, "", true);
        locks.set(ItemKind::Live, "Sport", true);
        locks.set(ItemKind::Live, "Sport", false);
        let value = locks.to_value();
        assert_eq!(Locks::from_value(&value), locks);
        assert!(locks.series.contains(""));

        let pairs = [
            ("locks.7".to_owned(), value),
            ("locks.x".to_owned(), "l\tNo".to_owned()),
            ("pin".to_owned(), "aa$bb".to_owned()),
        ];
        let s = Settings::from_pairs(&pairs, Language::Italian);
        assert_eq!(s.locks.len(), 1);
        assert!(s.locks[&7].live.contains("Adulti"));
        assert!(s.has_pin());
        assert_eq!(Locks::from_value("x\tA\nrotta"), Locks::default());
    }

    #[test]
    fn language_codes() {
        assert_eq!(Language::from_code("it-IT"), Some(Language::Italian));
        assert_eq!(Language::from_code("en_US"), Some(Language::English));
        assert_eq!(Language::from_code("de"), None);
        assert_eq!(
            Language::from_index(Language::English.index()),
            Language::English
        );
    }
}
