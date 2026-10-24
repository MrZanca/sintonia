//! Segreti nel portachiavi del sistema (Gestione credenziali di Windows, Portachiavi di macOS,
//! Secret Service su Linux).
//!
//! Due tipi di segreti: la chiave che cifra l'archivio ([`database_key`]) e le password degli
//! account Xtream ([`SecretStore`], una voce per sorgente). Il resto del codice usa il tratto
//! [`SecretStore`], così i test possono usare [`MemorySecrets`] senza toccare il sistema.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use crate::crypto::{key_from_hex, key_to_hex, Cipher, CryptoError, KEY_LEN};

/// Servizio delle voci nel portachiavi (con un profilo di prova: `Sintonia-<profilo>`).
pub const SERVICE: &str = "Sintonia";
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
pub fn database_key(service: &str) -> Result<([u8; KEY_LEN], bool), SecretsError> {
    let entry = keyring::Entry::new(service, DATABASE_KEY)?;
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

/// Nome della voce con la password di una sorgente.
pub fn source_password_name(source_id: i64) -> String {
    format!("sorgente-{source_id}")
}

/// Un deposito di segreti (testo) per nome.
pub trait SecretStore: Send + Sync + std::fmt::Debug {
    /// Legge un segreto (`None` se non c'è).
    fn get(&self, name: &str) -> Result<Option<String>, SecretsError>;
    /// Salva un segreto.
    fn set(&self, name: &str, value: &str) -> Result<(), SecretsError>;
    /// Elimina un segreto (nessun errore se non c'era).
    fn delete(&self, name: &str) -> Result<(), SecretsError>;
}

/// Il portachiavi del sistema; `service` è il gruppo delle voci (di solito [`SERVICE`]).
#[derive(Debug, Clone)]
pub struct SystemKeyring {
    pub service: String,
}

impl SecretStore for SystemKeyring {
    fn get(&self, name: &str) -> Result<Option<String>, SecretsError> {
        match keyring::Entry::new(&self.service, name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretsError> {
        keyring::Entry::new(&self.service, name)?.set_password(value)?;
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), SecretsError> {
        match keyring::Entry::new(&self.service, name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

/// Segreti in memoria, per i test e gli strumenti di sviluppo.
#[derive(Default)]
pub struct MemorySecrets {
    values: Mutex<HashMap<String, String>>,
}

impl std::fmt::Debug for MemorySecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MemorySecrets(..)")
    }
}

impl SecretStore for MemorySecrets {
    fn get(&self, name: &str) -> Result<Option<String>, SecretsError> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned())
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretsError> {
        self.values
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), SecretsError> {
        self.values
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_secrets_round_trip() {
        let secrets = MemorySecrets::default();
        let name = source_password_name(7);
        assert_eq!(name, "sorgente-7");
        assert_eq!(secrets.get(&name).unwrap(), None);
        secrets.set(&name, "segreta").unwrap();
        assert_eq!(secrets.get(&name).unwrap().as_deref(), Some("segreta"));
        secrets.delete(&name).unwrap();
        secrets.delete(&name).unwrap();
        assert_eq!(secrets.get(&name).unwrap(), None);
        assert!(!format!("{secrets:?}").contains("segreta"));
    }
}
