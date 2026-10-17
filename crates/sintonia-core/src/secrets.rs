//! Segreti nel portachiavi del sistema (Gestione credenziali di Windows, Portachiavi di macOS,
//! Secret Service su Linux).
//!
//! Per ora c'è un solo segreto: la chiave che cifra gli indirizzi nel database. Le credenziali
//! Xtream (M2) passeranno da qui.

use crate::crypto::{key_from_hex, key_to_hex, Cipher, CryptoError, KEY_LEN};

const SERVICE: &str = "Sintonia";
const DATABASE_KEY: &str = "chiave-archivio";

/// Errori del portachiavi.
#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    /// Il portachiavi del sistema non risponde o rifiuta l'accesso.
    #[error("portachiavi del sistema non disponibile: {0}")]
    Keyring(#[from] keyring::Error),
    /// Nel portachiavi c'è un valore che non è una chiave valida.
    #[error("la chiave dell'archivio nel portachiavi è danneggiata")]
    Corrupt,
    /// Errore nel generare la chiave.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

/// Restituisce la chiave dell'archivio, creandola al primo avvio.
///
/// `created` è `true` se la chiave è nuova: un archivio esistente cifrato con un'altra chiave
/// non sarà più leggibile, e l'app lo deve ricreare.
pub fn database_key() -> Result<([u8; KEY_LEN], bool), SecretsError> {
    let entry = keyring::Entry::new(SERVICE, DATABASE_KEY)?;
    match entry.get_password() {
        Ok(text) => key_from_hex(&text)
            .map(|key| (key, false))
            .ok_or(SecretsError::Corrupt),
        Err(keyring::Error::NoEntry) => {
            let key = Cipher::generate_key()?;
            entry.set_password(&key_to_hex(&key))?;
            Ok((key, true))
        }
        Err(err) => Err(err.into()),
    }
}
