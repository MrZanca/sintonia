//! Segreti in un file cifrato, per i sistemi senza un portachiavi raggiungibile da Rust (Android).
//!
//! Due file per servizio nella cartella dei dati: `<servizio>.chiave` è la chiave dei segreti
//! protetta da una chiave di sistema che non esce mai dal suo deposito ([`KeyWrap`]: su Android
//! l'Android Keystore), `<servizio>.segreti` è l'elenco dei segreti in JSON, cifrato con quella
//! chiave da [`Cipher`]. Così basta una sola operazione nel Keystore per avvio (che è lenta),
//! e la cifratura vera è quella già usata per l'archivio.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::crypto::{Cipher, KEY_LEN};
use crate::secrets::SecretsError;

/// La chiave di sistema che protegge la chiave dei segreti.
pub(crate) trait KeyWrap: Send + Sync {
    /// Cifra la chiave dei segreti.
    fn wrap(&self, key: &[u8; KEY_LEN]) -> Result<Vec<u8>, SecretsError>;
    /// Decifra la chiave dei segreti. `Ok(None)`: la chiave di sistema non c'è più (dati
    /// dell'app cancellati a metà, ripristino da un altro telefono) e i segreti sono persi.
    fn unwrap(&self, blob: &[u8]) -> Result<Option<[u8; KEY_LEN]>, SecretsError>;
}

/// Il deposito di un servizio. I segreti si rileggono dal file a ogni operazione (sono pochi e
/// rari); la chiave decifrata resta in memoria.
pub(crate) struct Vault<W> {
    dir: PathBuf,
    service: String,
    wrap: W,
    cipher: Mutex<Option<Cipher>>,
}

impl<W> std::fmt::Debug for Vault<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl<W: KeyWrap> Vault<W> {
    pub(crate) fn new(dir: &Path, service: &str, wrap: W) -> Self {
        Self {
            dir: dir.to_path_buf(),
            service: service.to_owned(),
            wrap,
            cipher: Mutex::new(None),
        }
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join(format!("{}.chiave", self.service))
    }

    fn values_path(&self) -> PathBuf {
        self.dir.join(format!("{}.segreti", self.service))
    }

    /// Legge un segreto.
    pub(crate) fn get(&self, name: &str) -> Result<Option<String>, SecretsError> {
        let mut cipher = self.cipher.lock().unwrap_or_else(PoisonError::into_inner);
        let values = self.load(&mut cipher)?;
        Ok(values.get(name).cloned())
    }

    /// Salva un segreto.
    pub(crate) fn set(&self, name: &str, value: &str) -> Result<(), SecretsError> {
        let mut cipher = self.cipher.lock().unwrap_or_else(PoisonError::into_inner);
        let mut values = self.load(&mut cipher)?;
        values.insert(name.to_owned(), value.to_owned());
        self.save(&mut cipher, &values)
    }

    /// Elimina un segreto (nessun errore se non c'era).
    pub(crate) fn delete(&self, name: &str) -> Result<(), SecretsError> {
        let mut cipher = self.cipher.lock().unwrap_or_else(PoisonError::into_inner);
        let mut values = self.load(&mut cipher)?;
        if values.remove(name).is_some() {
            self.save(&mut cipher, &values)?;
        }
        Ok(())
    }

    /// I segreti salvati (vuoto se non ce ne sono ancora).
    fn load(&self, cipher: &mut Option<Cipher>) -> Result<BTreeMap<String, String>, SecretsError> {
        let sealed = match std::fs::read(self.values_path()) {
            Ok(sealed) => sealed,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(err) => return Err(storage(err)),
        };
        let Some(current) = self.cipher(cipher, false)? else {
            // file dei segreti senza la sua chiave: illeggibile, come se non ci fosse
            log::warn!(
                "segreti di {} senza chiave: si ripartono vuoti",
                self.service
            );
            return Ok(BTreeMap::new());
        };
        let text = current
            .decrypt(&sealed)
            .map_err(|_| SecretsError::Corrupt)?;
        serde_json::from_str(&text).map_err(|_| SecretsError::Corrupt)
    }

    fn save(
        &self,
        cipher: &mut Option<Cipher>,
        values: &BTreeMap<String, String>,
    ) -> Result<(), SecretsError> {
        let current = self
            .cipher(cipher, true)?
            .ok_or_else(|| SecretsError::Platform("chiave dei segreti non creata".into()))?;
        let text = serde_json::to_string(values).map_err(storage)?;
        write_atomic(&self.values_path(), &current.encrypt(&text)?)
    }

    /// La chiave dei segreti: dalla memoria, dal file o (se `create`) nuova. Se la chiave di
    /// sistema è sparita il file della chiave non serve più e si toglie insieme ai segreti.
    fn cipher(
        &self,
        cached: &mut Option<Cipher>,
        create: bool,
    ) -> Result<Option<Cipher>, SecretsError> {
        if let Some(cipher) = cached {
            return Ok(Some(cipher.clone()));
        }
        let path = self.key_path();
        let key = match std::fs::read(&path) {
            Ok(blob) => match self.wrap.unwrap(&blob)? {
                Some(key) => Some(key),
                None => {
                    log::warn!("chiave di sistema di {} sparita", self.service);
                    remove_if_present(&self.values_path())?;
                    remove_if_present(&path)?;
                    None
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(storage(err)),
        };
        let key = match key {
            Some(key) => key,
            None if create => {
                // segreti rimasti da una chiave precedente non si potrebbero più leggere
                remove_if_present(&self.values_path())?;
                let key = Cipher::generate_key()?;
                write_atomic(&path, &self.wrap.wrap(&key)?)?;
                key
            }
            None => return Ok(None),
        };
        let cipher = Cipher::new(&key);
        *cached = Some(cipher.clone());
        Ok(Some(cipher))
    }
}

fn storage(err: impl std::fmt::Display) -> SecretsError {
    SecretsError::Platform(err.to_string())
}

fn remove_if_present(path: &Path) -> Result<(), SecretsError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(storage(err)),
    }
}

/// Scrive accanto e poi rinomina: un'interruzione a metà lascia il file vecchio intero.
fn write_atomic(path: &Path, data: &[u8]) -> Result<(), SecretsError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(storage)?;
    }
    let partial = path.with_extension("tmp");
    let mut file = std::fs::File::create(&partial).map_err(storage)?;
    file.write_all(data).map_err(storage)?;
    file.sync_all().map_err(storage)?;
    drop(file);
    std::fs::rename(&partial, path).map_err(storage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// Finta chiave di sistema: XOR con un byte; `lost` simula la chiave sparita.
    #[derive(Clone, Default)]
    struct FakeWrap {
        lost: Arc<AtomicBool>,
    }

    impl KeyWrap for FakeWrap {
        fn wrap(&self, key: &[u8; KEY_LEN]) -> Result<Vec<u8>, SecretsError> {
            Ok(key.iter().map(|b| b ^ 0x5a).collect())
        }

        fn unwrap(&self, blob: &[u8]) -> Result<Option<[u8; KEY_LEN]>, SecretsError> {
            if self.lost.load(Ordering::SeqCst) {
                return Ok(None);
            }
            let key: Vec<u8> = blob.iter().map(|b| b ^ 0x5a).collect();
            key.try_into().map(Some).map_err(|_| SecretsError::Corrupt)
        }
    }

    #[test]
    fn values_survive_a_restart_and_stay_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path(), "Sintonia", FakeWrap::default());
        assert_eq!(vault.get("sorgente-1").unwrap(), None);
        vault.set("sorgente-1", "password-segreta").unwrap();
        vault.set("sorgente-2", "altra").unwrap();
        vault.delete("sorgente-2").unwrap();
        vault.delete("mai-esistita").unwrap();

        let again = Vault::new(dir.path(), "Sintonia", FakeWrap::default());
        assert_eq!(
            again.get("sorgente-1").unwrap().as_deref(),
            Some("password-segreta")
        );
        assert_eq!(again.get("sorgente-2").unwrap(), None);
        let raw = std::fs::read(dir.path().join("Sintonia.segreti")).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("segreta"));
        assert!(!String::from_utf8_lossy(&raw).contains("sorgente"));
        assert!(!format!("{again:?}").contains("segreta"));
    }

    #[test]
    fn services_are_separate() {
        let dir = tempfile::tempdir().unwrap();
        let real = Vault::new(dir.path(), "Sintonia", FakeWrap::default());
        let test = Vault::new(dir.path(), "Sintonia-prova", FakeWrap::default());
        real.set("chiave-archivio", "vera").unwrap();
        assert_eq!(test.get("chiave-archivio").unwrap(), None);
    }

    #[test]
    fn lost_system_key_starts_over_empty() {
        let dir = tempfile::tempdir().unwrap();
        let wrap = FakeWrap::default();
        Vault::new(dir.path(), "Sintonia", wrap.clone())
            .set("chiave-archivio", "vecchia")
            .unwrap();
        wrap.lost.store(true, Ordering::SeqCst);
        let vault = Vault::new(dir.path(), "Sintonia", wrap.clone());
        assert_eq!(vault.get("chiave-archivio").unwrap(), None);
        assert!(!dir.path().join("Sintonia.chiave").exists());
        wrap.lost.store(false, Ordering::SeqCst);
        vault.set("chiave-archivio", "nuova").unwrap();
        assert_eq!(
            vault.get("chiave-archivio").unwrap().as_deref(),
            Some("nuova")
        );
    }

    #[test]
    fn damaged_values_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path(), "Sintonia", FakeWrap::default());
        vault.set("a", "b").unwrap();
        std::fs::write(dir.path().join("Sintonia.segreti"), b"rovinato").unwrap();
        let again = Vault::new(dir.path(), "Sintonia", FakeWrap::default());
        assert!(matches!(again.get("a"), Err(SecretsError::Corrupt)));
    }
}
