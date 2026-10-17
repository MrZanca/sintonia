//! Cifratura dei dati sensibili nel database (indirizzi dei canali e delle playlist).
//!
//! XChaCha20-Poly1305 con un nonce casuale per ogni valore: `nonce (24 byte) || testo cifrato`.
//! La chiave da 32 byte sta nel portachiavi del sistema (vedi [`crate::secrets`]), mai su disco.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

/// Lunghezza della chiave in byte.
pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;

/// Errori di cifratura.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// Il generatore casuale del sistema non ha risposto.
    #[error("generatore casuale non disponibile")]
    Random,
    /// Dato corrotto o cifrato con un'altra chiave.
    #[error("dato cifrato non valido (chiave diversa o archivio danneggiato)")]
    Invalid,
}

/// Una chiave pronta all'uso. Il `Debug` non mostra la chiave.
#[derive(Clone)]
pub struct Cipher {
    aead: XChaCha20Poly1305,
}

impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cipher(..)")
    }
}

impl Cipher {
    /// Crea il cifrario da una chiave.
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        Self {
            aead: XChaCha20Poly1305::new(&(*key).into()),
        }
    }

    /// Genera una chiave nuova.
    pub fn generate_key() -> Result<[u8; KEY_LEN], CryptoError> {
        let mut key = [0u8; KEY_LEN];
        getrandom::fill(&mut key).map_err(|_| CryptoError::Random)?;
        Ok(key)
    }

    /// Cifra un testo.
    pub fn encrypt(&self, plain: &str) -> Result<Vec<u8>, CryptoError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| CryptoError::Random)?;
        let sealed = self
            .aead
            .encrypt(&XNonce::from(nonce), plain.as_bytes())
            .map_err(|_| CryptoError::Invalid)?;
        let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// Decifra un valore prodotto da [`Self::encrypt`].
    pub fn decrypt(&self, data: &[u8]) -> Result<String, CryptoError> {
        if data.len() < NONCE_LEN {
            return Err(CryptoError::Invalid);
        }
        let (nonce, sealed) = data.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Invalid)?;
        let plain = self
            .aead
            .decrypt(&XNonce::from(nonce), sealed)
            .map_err(|_| CryptoError::Invalid)?;
        String::from_utf8(plain).map_err(|_| CryptoError::Invalid)
    }
}

/// Chiave in esadecimale (per il portachiavi, che conserva testo).
pub(crate) fn key_to_hex(key: &[u8; KEY_LEN]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

/// Inverso di [`key_to_hex`].
pub(crate) fn key_from_hex(text: &str) -> Option<[u8; KEY_LEN]> {
    let text = text.trim();
    if text.len() != KEY_LEN * 2 {
        return None;
    }
    let mut key = [0u8; KEY_LEN];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_random_nonce() {
        let cipher = Cipher::new(&Cipher::generate_key().unwrap());
        let a = cipher.encrypt("http://h/live/u/p/1.ts").unwrap();
        let b = cipher.encrypt("http://h/live/u/p/1.ts").unwrap();
        assert_ne!(a, b, "ogni valore ha il suo nonce");
        assert_eq!(cipher.decrypt(&a).unwrap(), "http://h/live/u/p/1.ts");
        assert!(!String::from_utf8_lossy(&a).contains("live"));
    }

    #[test]
    fn wrong_key_or_tampering_is_rejected() {
        let one = Cipher::new(&[1; KEY_LEN]);
        let two = Cipher::new(&[2; KEY_LEN]);
        let mut sealed = one.encrypt("segreto").unwrap();
        assert!(two.decrypt(&sealed).is_err());
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(one.decrypt(&sealed).is_err());
        assert!(one.decrypt(&[0; 5]).is_err());
    }

    #[test]
    fn hex_round_trip() {
        let key = Cipher::generate_key().unwrap();
        assert_eq!(key_from_hex(&key_to_hex(&key)), Some(key));
        assert_eq!(key_from_hex("zz"), None);
    }

    #[test]
    fn debug_hides_the_key() {
        assert_eq!(format!("{:?}", Cipher::new(&[7; KEY_LEN])), "Cipher(..)");
    }
}
