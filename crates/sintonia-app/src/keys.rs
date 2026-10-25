//! I tasti delle azioni (Impostazioni › Tasti): quelli di partenza, come si salvano e come si
//! chiamano sullo schermo.
//!
//! Un tasto è il testo che Slint mette in `KeyEvent.text`: una lettera, un simbolo, o un carattere
//! dell'area privata Unicode per i tasti speciali (frecce, F1…, Home…). I tasti dei telecomandi che
//! Slint non conosce arrivano come F13…F24 (vedi `remote.rs`).

use slint::platform::Key;

use crate::settings::Language;

/// Le azioni, nell'ordine dell'interfaccia: nome nell'archivio e tasto di partenza (con mouse e
/// tastiera; quelli del telecomando in [`defaults`]).
pub const ACTIONS: [(&str, &str); 12] = [
    ("fullscreen", "f"),
    ("pip", "p"),
    ("mute", "m"),
    ("volume_up", "+"),
    ("volume_down", "-"),
    ("favorite", "s"),
    ("guide", "g"),
    ("home", "h"),
    ("back", ""),
    ("search", ""),
    ("channel_next", ""),
    ("channel_prev", ""),
];

/// I tasti in più dei telecomandi, come arrivano (vedi `remote.rs`), con il loro nome.
const EXTRA_KEYS: [(Key, &str, &str); 12] = [
    (Key::F13, "Preferiti", "Favorites"),
    (Key::F14, "Cerca", "Search"),
    (Key::F15, "Aggiorna", "Refresh"),
    (Key::F16, "Avanti", "Forward"),
    (Key::F17, "Traccia succ.", "Next track"),
    (Key::F18, "Traccia prec.", "Prev. track"),
    (Key::F19, "Stop", "Stop"),
    (Key::F20, "Posta", "Mail"),
    (Key::F21, "Home (telecomando)", "Home (remote)"),
    (Key::F22, "Lettore", "Media player"),
    (Key::F23, "Tasto «e»", "«e» key"),
    (Key::F24, "Guida TV", "TV guide"),
];

/// Chiave dell'impostazione dell'azione `index`: mouse e tastiera e telecomando hanno tasti propri.
pub fn setting_key(index: usize, remote: bool) -> Option<String> {
    let mode = if remote { "remote." } else { "" };
    ACTIONS
        .get(index)
        .map(|(name, _)| format!("key.{mode}{name}"))
}

/// I tasti di partenza. Con il telecomando (provati su un Esynic): lettore multimediale per lo
/// schermo intero, posta per la Guida, il tasto Home (manda «applicazione 1») per la Home, il
/// tasto «e» (manda «home del browser») per indietro, cerca per la ricerca, traccia seguente e
/// precedente per cambiare canale.
pub fn defaults(remote: bool) -> Vec<String> {
    let mut keys: Vec<String> = ACTIONS.iter().map(|(_, key)| (*key).to_owned()).collect();
    if remote {
        let text = |k: Key| slint::SharedString::from(k).to_string();
        keys[0] = text(Key::F22);
        keys[6] = text(Key::F20);
        keys[7] = text(Key::F21);
        keys[8] = text(Key::F23);
        keys[9] = text(Key::F14);
        keys[10] = text(Key::F17);
        keys[11] = text(Key::F18);
    }
    keys
}

/// La variante maiuscola di una lettera (con Maiusc o il blocco maiuscole la lettera arriva così).
pub fn alternate(key: &str) -> String {
    let upper = key.to_uppercase();
    if upper == key {
        String::new()
    } else {
        upper
    }
}

/// Assegna `key` all'azione `index`; se un'altra azione aveva lo stesso tasto, resta senza.
/// Restituisce le azioni cambiate.
pub fn assign(keys: &mut [String], index: usize, key: &str) -> Vec<usize> {
    if index >= keys.len() {
        return Vec::new();
    }
    let key = key.to_lowercase();
    let mut changed = vec![index];
    for (i, other) in keys.iter_mut().enumerate() {
        if i != index && !key.is_empty() && *other == key {
            other.clear();
            changed.push(i);
        }
    }
    keys[index] = key;
    changed
}

/// Il nome del tasto da mostrare nelle Impostazioni.
pub fn label(key: &str, language: Language) -> String {
    let it = language == Language::Italian;
    let Some(c) = key.chars().next() else {
        return String::new();
    };
    let named = |k: Key| key == slint::SharedString::from(k).as_str();
    let special = [
        (Key::Space, "Spazio", "Space"),
        (Key::Escape, "Esc", "Esc"),
        (Key::Delete, "Canc", "Delete"),
        (Key::Insert, "Ins", "Insert"),
        (Key::Home, "Inizio", "Home"),
        (Key::End, "Fine", "End"),
        (Key::PageUp, "Pag ↑", "Page Up"),
        (Key::PageDown, "Pag ↓", "Page Down"),
        (Key::Menu, "Menu", "Menu"),
        (Key::Pause, "Pausa", "Pause"),
        (Key::ScrollLock, "Bloc Scorr", "Scroll Lock"),
    ];
    for (k, it_name, en_name) in special {
        if named(k) {
            return if it { it_name } else { en_name }.to_owned();
        }
    }
    let function_keys = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ];
    if let Some(n) = function_keys.iter().position(|k| named(*k)) {
        return format!("F{}", n + 1);
    }
    if let Some((_, it_name, en_name)) = EXTRA_KEYS.iter().find(|(k, _, _)| named(*k)) {
        return if it { *it_name } else { *en_name }.to_owned();
    }
    // altri tasti speciali senza nome: meglio dirlo che mostrare un carattere invisibile
    if ('\u{f700}'..='\u{f7ff}').contains(&c) || c.is_control() {
        return if it { "Tasto speciale" } else { "Special key" }.to_owned();
    }
    key.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_names() {
        assert_eq!(defaults(false).len(), ACTIONS.len());
        assert_eq!(defaults(false)[8], "");
        assert_eq!(
            defaults(true)[8],
            slint::SharedString::from(Key::F23).as_str()
        );
        assert_eq!(setting_key(0, false).as_deref(), Some("key.fullscreen"));
        assert_eq!(setting_key(2, true).as_deref(), Some("key.remote.mute"));
        assert_eq!(setting_key(99, false), None);
        assert_eq!(alternate("f"), "F");
        assert_eq!(alternate("+"), "");
    }

    #[test]
    fn assigning_moves_a_taken_key() {
        let mut keys = defaults(false);
        // «m» era del muto: passa allo schermo intero e il muto resta senza tasto
        let changed = assign(&mut keys, 0, "M");
        assert_eq!(keys[0], "m");
        assert_eq!(keys[2], "");
        assert_eq!(changed, vec![0, 2]);
        assert!(assign(&mut keys, 50, "x").is_empty());
    }

    #[test]
    fn labels() {
        assert_eq!(label("f", Language::Italian), "F");
        assert_eq!(label("+", Language::English), "+");
        assert_eq!(label(" ", Language::Italian), "Spazio");
        let f13 = slint::SharedString::from(Key::F13);
        assert_eq!(label(&f13, Language::Italian), "Preferiti");
        let f5 = slint::SharedString::from(Key::F5);
        assert_eq!(label(&f5, Language::English), "F5");
        assert_eq!(label("", Language::Italian), "");
    }
}
