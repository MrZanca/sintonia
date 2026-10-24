//! PIN del controllo genitori: si salva solo un'impronta con sale (`"<sale>$<impronta>"`, in
//! esadecimale), mai le cifre.
//!
//! Un PIN di quattro cifre si indovina comunque provando tutte le combinazioni: l'impronta serve
//! solo a non lasciarlo leggibile nell'archivio. Il blocco protegge dai bambini, non da chi ha
//! accesso ai file del PC.

use sha2::{Digest, Sha256};

/// Cifre minime e massime.
pub const MIN_LEN: usize = 4;
pub const MAX_LEN: usize = 8;
/// Passate di SHA-256: rendono la verifica lenta quanto basta (qualche millisecondo).
const ROUNDS: u32 = 20_000;
const SALT_LEN: usize = 16;

/// Perché un PIN non va bene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// Non è fatto di sole cifre o ha una lunghezza fuori dai limiti.
    #[error("il PIN deve avere da {MIN_LEN} a {MAX_LEN} cifre")]
    Invalid,
    /// Il generatore di numeri casuali del sistema non risponde.
    #[error("generatore casuale non disponibile")]
    Random,
}

/// Il PIN ha solo cifre e una lunghezza ammessa.
pub fn is_valid(pin: &str) -> bool {
    (MIN_LEN..=MAX_LEN).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_digit())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn digest(salt: &[u8], pin: &str) -> String {
    let mut hash: [u8; 32] = Sha256::new()
        .chain_update(salt)
        .chain_update(pin.as_bytes())
        .finalize()
        .into();
    for _ in 1..ROUNDS {
        hash = Sha256::new()
            .chain_update(hash)
            .chain_update(salt)
            .finalize()
            .into();
    }
    to_hex(&hash)
}

/// L'impronta da salvare per `pin`.
pub fn hash(pin: &str) -> Result<String, PinError> {
    if !is_valid(pin) {
        return Err(PinError::Invalid);
    }
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|_| PinError::Random)?;
    Ok(format!("{}${}", to_hex(&salt), digest(&salt, pin)))
}

/// `pin` corrisponde all'impronta salvata.
pub fn verify(pin: &str, stored: &str) -> bool {
    let Some((salt, expected)) = stored.split_once('$') else {
        return false;
    };
    let Some(salt) = from_hex(salt) else {
        return false;
    };
    is_valid(pin) && digest(&salt, pin) == expected
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let stored = hash("1234").expect("impronta");
        assert!(!stored.contains("1234"));
        assert!(verify("1234", &stored));
        assert!(!verify("1235", &stored));
        assert!(!verify("", &stored));
        // sale diverso a ogni impronta
        assert_ne!(hash("1234").expect("impronta"), stored);
    }

    #[test]
    fn only_digits_of_allowed_length() {
        assert!(is_valid("0000"));
        assert!(is_valid("12345678"));
        assert!(!is_valid("123"));
        assert!(!is_valid("123456789"));
        assert!(!is_valid("12a4"));
        assert!(!is_valid("１２３４"), "cifre non ASCII");
        assert_eq!(hash("12"), Err(PinError::Invalid));
    }

    #[test]
    fn broken_stored_values() {
        assert!(!verify("1234", ""));
        assert!(!verify("1234", "zz$00"));
        assert!(!verify("1234", "abc$00"));
    }
}
