//! Impostazioni dell'utente: chiavi e valori salvati nell'archivio (tabella `settings`).

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
    /// User-agent generale (vuoto = quello di Sintonia).
    pub user_agent: Option<String>,
    /// Nome dell'ultimo canale visto (gli identificativi cambiano a ogni aggiornamento).
    pub last_channel: Option<String>,
    /// Larghezza del mini player (pixel logici).
    pub pip_width: Option<f32>,
}

impl Settings {
    pub fn defaults(language: Language) -> Self {
        Self {
            language,
            reconnect: true,
            fullscreen_start: false,
            user_agent: None,
            last_channel: None,
            pip_width: None,
        }
    }

    /// Dalle coppie salvate; le chiavi sconosciute si ignorano.
    pub fn from_pairs(pairs: &[(String, String)], default_language: Language) -> Self {
        let mut settings = Self::defaults(default_language);
        for (key, value) in pairs {
            settings.set(key, value);
        }
        settings
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
            "user_agent" => {
                self.user_agent = Some(value.trim())
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned);
            }
            "last_channel" => self.last_channel = Some(value.to_owned()),
            "pip_width" => self.pip_width = value.parse().ok().filter(|w: &f32| w.is_finite()),
            _ => {}
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
            ("user_agent".to_owned(), "  ".to_owned()),
            ("sconosciuta".to_owned(), "x".to_owned()),
        ];
        let s = Settings::from_pairs(&pairs, Language::Italian);
        assert_eq!(s.language, Language::English);
        assert!(!s.reconnect);
        assert_eq!(s.user_agent, None);
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
