//! Il collegamento con la parte Java di Android: cartelle dell'app, Android Keystore per i
//! segreti, proxy di sistema e autorità di certificazione.
//!
//! Da Rust non si raggiunge niente di tutto questo senza la JVM: l'app chiama [`init`] una volta
//! all'avvio con la JavaVM e l'activity di `android-activity`, e da lì il core se la cava da solo.
//! Il resto del core non sa su che sistema gira: usa [`crate::secrets`] e il client HTTP di
//! [`crate::service::http_client`] come su Windows.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::OnceLock;

use jni::objects::{JByteArray, JObject, JObjectArray, JString, JValue};
use jni::{jni_sig, jni_str, Env, JavaVM};

use crate::crypto::KEY_LEN;
use crate::secrets::SecretsError;
use crate::vault::KeyWrap;

/// Errori del collegamento con Java.
#[derive(Debug, thiserror::Error)]
pub enum AndroidError {
    /// [`init`] non è stato chiamato.
    #[error("collegamento con Android non inizializzato")]
    NotInitialized,
    /// Una chiamata Java è fallita (eccezione o errore di JNI).
    #[error("chiamata Java non riuscita: {0}")]
    Jni(String),
}

impl From<jni::errors::Error> for AndroidError {
    fn from(err: jni::errors::Error) -> Self {
        Self::Jni(describe(&err))
    }
}

/// Un errore di JNI senza la pila delle chiamate Java, che nei log non serve e allunga.
fn describe(err: &jni::errors::Error) -> String {
    match err {
        jni::errors::Error::CaughtJavaException { name, msg, .. } => format!("{name}: {msg}"),
        other => other.to_string(),
    }
}

struct Runtime {
    vm: JavaVM,
    files: PathBuf,
    cache: PathBuf,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Collega il core alla JVM. Va chiamata prima di tutto il resto, con i puntatori di
/// `AndroidApp::vm_as_ptr()` e `AndroidApp::activity_as_ptr()`; le chiamate successive non
/// fanno niente.
///
/// # Safety
///
/// `vm` deve essere la `JavaVM*` del processo e `activity` un riferimento JNI valido a un
/// `android.content.Context` (l'activity), entrambi vivi per tutta la chiamata.
pub unsafe fn init(vm: *mut c_void, activity: *mut c_void) -> Result<(), AndroidError> {
    if RUNTIME.get().is_some() {
        return Ok(());
    }
    if vm.is_null() || activity.is_null() {
        return Err(AndroidError::NotInitialized);
    }
    // SAFETY: il chiamante garantisce che `vm` è la JavaVM del processo (non nulla, controllato).
    let vm = unsafe { JavaVM::from_raw(vm.cast()) };
    let runtime = vm.attach_current_thread(|env| -> Result<Runtime, AndroidError> {
        // SAFETY: `activity` è un riferimento valido per tutta la chiamata (contratto della
        // funzione). È globale e non locale, ma `JObject` non lo libera mai: lo si usa solo
        // per chiamarci sopra metodi, cosa che JNI permette con ogni tipo di riferimento.
        let activity = unsafe { JObject::from_raw(env, activity.cast()) };
        Ok(Runtime {
            vm: vm.clone(),
            files: dir_of(env, &activity, jni_str!("getFilesDir"))?,
            cache: dir_of(env, &activity, jni_str!("getCacheDir"))?,
        })
    })?;
    log::info!(
        "Android: dati in {}, cache in {}",
        runtime.files.display(),
        runtime.cache.display()
    );
    let _ = RUNTIME.set(runtime);
    Ok(())
}

fn runtime() -> Result<&'static Runtime, AndroidError> {
    RUNTIME.get().ok_or(AndroidError::NotInitialized)
}

/// Le cartelle dell'app: dati (`Context.getFilesDir`, la `internal_data_path` di
/// `android-activity`) e cache (`Context.getCacheDir`, che Android può svuotare da sé).
/// Sono private dell'app e spariscono con la disinstallazione.
pub fn app_dirs() -> Result<(PathBuf, PathBuf), AndroidError> {
    let runtime = runtime()?;
    Ok((runtime.files.clone(), runtime.cache.clone()))
}

/// Il percorso assoluto di un `java.io.File` restituito da un metodo del `Context`.
fn dir_of(
    env: &mut Env<'_>,
    context: &JObject<'_>,
    method: &jni::strings::JNIStr,
) -> Result<PathBuf, AndroidError> {
    let file = env
        .call_method(context, method, jni_sig!("()Ljava/io/File;"), &[])?
        .l()?;
    let path = env
        .call_method(
            &file,
            jni_str!("getAbsolutePath"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    Ok(PathBuf::from(string_of(env, path)?))
}

fn string_of(env: &mut Env<'_>, obj: JObject<'_>) -> Result<String, AndroidError> {
    let text = env.cast_local::<JString>(obj)?;
    Ok(text.try_to_string(env)?)
}

/// Esegue `f` con la JVM, da qualunque thread (anche quelli di tokio).
fn with_env<T>(f: impl FnOnce(&mut Env<'_>) -> Result<T, AndroidError>) -> Result<T, AndroidError> {
    runtime()?.vm.attach_current_thread(f)
}

/// Una proprietà di sistema di Java (`System.getProperty`), `None` se non c'è o è vuota.
pub(crate) fn system_property(name: &str) -> Result<Option<String>, AndroidError> {
    with_env(|env| {
        let key = env.new_string(name)?;
        let value = env
            .call_static_method(
                jni_str!("java/lang/System"),
                jni_str!("getProperty"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
                &[JValue::Object(&key)],
            )?
            .l()?;
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(string_of(env, value)?).filter(|v| !v.trim().is_empty()))
    })
}

/// Le cartelle con le autorità di certificazione di Android. Da Android 14 l'elenco aggiornabile
/// sta nel modulo Conscrypt.
const SYSTEM_CA_DIRS: [&str; 2] = [
    "/apex/com.android.conscrypt/cacerts",
    "/system/etc/security/cacerts",
];

/// La chiave AES nell'Android Keystore che protegge la chiave dei segreti di un servizio.
/// Non esce mai dal Keystore (sui telefoni che lo hanno, sta nell'hardware sicuro).
#[derive(Debug)]
pub(crate) struct Keystore {
    alias: String,
}

impl Keystore {
    pub(crate) fn new(service: &str) -> Self {
        Self {
            alias: service.to_owned(),
        }
    }

    /// La chiave del Keystore (`None` se non c'è e `create` è falso).
    fn key<'local>(
        &self,
        env: &mut Env<'local>,
        create: bool,
    ) -> Result<Option<JObject<'local>>, AndroidError> {
        let provider = env.new_string("AndroidKeyStore")?;
        let store = env
            .call_static_method(
                jni_str!("java/security/KeyStore"),
                jni_str!("getInstance"),
                jni_sig!("(Ljava/lang/String;)Ljava/security/KeyStore;"),
                &[JValue::Object(&provider)],
            )?
            .l()?;
        env.call_method(
            &store,
            jni_str!("load"),
            jni_sig!("(Ljava/security/KeyStore$LoadStoreParameter;)V"),
            &[JValue::Object(&JObject::null())],
        )?;
        let alias = env.new_string(&self.alias)?;
        let key = env
            .call_method(
                &store,
                jni_str!("getKey"),
                jni_sig!("(Ljava/lang/String;[C)Ljava/security/Key;"),
                &[JValue::Object(&alias), JValue::Object(&JObject::null())],
            )?
            .l()?;
        if !key.is_null() {
            return Ok(Some(key));
        }
        if !create {
            return Ok(None);
        }
        // PURPOSE_ENCRYPT | PURPOSE_DECRYPT; nessuna autenticazione dell'utente: l'app deve
        // poter leggere le password anche quando si avvia da sola (Android TV, nessun blocco)
        let builder = env.new_object(
            jni_str!("android/security/keystore/KeyGenParameterSpec$Builder"),
            jni_sig!("(Ljava/lang/String;I)V"),
            &[JValue::Object(&alias), JValue::Int(3)],
        )?;
        let gcm = string_array(env, "GCM")?;
        let builder = env
            .call_method(
                &builder,
                jni_str!("setBlockModes"),
                jni_sig!(
                    "([Ljava/lang/String;)Landroid/security/keystore/KeyGenParameterSpec$Builder;"
                ),
                &[JValue::Object(&gcm)],
            )?
            .l()?;
        let none = string_array(env, "NoPadding")?;
        let builder = env
            .call_method(
                &builder,
                jni_str!("setEncryptionPaddings"),
                jni_sig!(
                    "([Ljava/lang/String;)Landroid/security/keystore/KeyGenParameterSpec$Builder;"
                ),
                &[JValue::Object(&none)],
            )?
            .l()?;
        let builder = env
            .call_method(
                &builder,
                jni_str!("setKeySize"),
                jni_sig!("(I)Landroid/security/keystore/KeyGenParameterSpec$Builder;"),
                &[JValue::Int(256)],
            )?
            .l()?;
        let spec = env
            .call_method(
                &builder,
                jni_str!("build"),
                jni_sig!("()Landroid/security/keystore/KeyGenParameterSpec;"),
                &[],
            )?
            .l()?;
        let aes = env.new_string("AES")?;
        let generator = env
            .call_static_method(
                jni_str!("javax/crypto/KeyGenerator"),
                jni_str!("getInstance"),
                jni_sig!("(Ljava/lang/String;Ljava/lang/String;)Ljavax/crypto/KeyGenerator;"),
                &[JValue::Object(&aes), JValue::Object(&provider)],
            )?
            .l()?;
        env.call_method(
            &generator,
            jni_str!("init"),
            jni_sig!("(Ljava/security/spec/AlgorithmParameterSpec;)V"),
            &[JValue::Object(&spec)],
        )?;
        let key = env
            .call_method(
                &generator,
                jni_str!("generateKey"),
                jni_sig!("()Ljavax/crypto/SecretKey;"),
                &[],
            )?
            .l()?;
        log::info!("creata la chiave del Keystore per {}", self.alias);
        Ok(Some(key))
    }
}

fn string_array<'local>(
    env: &mut Env<'local>,
    value: &str,
) -> Result<JObjectArray<'local>, AndroidError> {
    let value = env.new_string(value)?;
    Ok(env.new_object_array(1, jni_str!("java/lang/String"), &value)?)
}

fn aes_gcm<'local>(env: &mut Env<'local>) -> Result<JObject<'local>, AndroidError> {
    let name = env.new_string("AES/GCM/NoPadding")?;
    Ok(env
        .call_static_method(
            jni_str!("javax/crypto/Cipher"),
            jni_str!("getInstance"),
            jni_sig!("(Ljava/lang/String;)Ljavax/crypto/Cipher;"),
            &[JValue::Object(&name)],
        )?
        .l()?)
}

fn do_final(
    env: &mut Env<'_>,
    cipher: &JObject<'_>,
    input: &[u8],
) -> Result<Vec<u8>, AndroidError> {
    let input = env.byte_array_from_slice(input)?;
    let output = env
        .call_method(
            cipher,
            jni_str!("doFinal"),
            jni_sig!("([B)[B"),
            &[JValue::Object(&input)],
        )?
        .l()?;
    let output = env.cast_local::<JByteArray>(output)?;
    Ok(env.convert_byte_array(&output)?)
}

/// Lunghezza del vettore iniziale di AES-GCM scelto dal Keystore.
const IV_LEN: usize = 12;

impl KeyWrap for Keystore {
    /// `vettore iniziale (12 byte) || chiave cifrata con l'etichetta GCM`. Il vettore lo
    /// sceglie il Keystore: per le sue chiavi non accetta vettori scelti da fuori.
    fn wrap(&self, key: &[u8; KEY_LEN]) -> Result<Vec<u8>, SecretsError> {
        with_env(|env| {
            let secret = self.key(env, true)?.ok_or(AndroidError::NotInitialized)?;
            let cipher = aes_gcm(env)?;
            env.call_method(
                &cipher,
                jni_str!("init"),
                jni_sig!("(ILjava/security/Key;)V"),
                // Cipher.ENCRYPT_MODE
                &[JValue::Int(1), JValue::Object(&secret)],
            )?;
            let iv = env
                .call_method(&cipher, jni_str!("getIV"), jni_sig!("()[B"), &[])?
                .l()?;
            let iv = env.convert_byte_array(&env.cast_local::<JByteArray>(iv)?)?;
            if iv.len() != IV_LEN {
                return Err(AndroidError::Jni(format!(
                    "vettore iniziale di {} byte",
                    iv.len()
                )));
            }
            let mut blob = iv;
            blob.extend(do_final(env, &cipher, key)?);
            Ok(blob)
        })
        .map_err(keystore_error)
    }

    fn unwrap(&self, blob: &[u8]) -> Result<Option<[u8; KEY_LEN]>, SecretsError> {
        if blob.len() <= IV_LEN {
            return Err(SecretsError::Corrupt);
        }
        let (iv, sealed) = blob.split_at(IV_LEN);
        let plain = with_env(|env| {
            let Some(secret) = self.key(env, false)? else {
                return Ok(None);
            };
            let iv = env.byte_array_from_slice(iv)?;
            let spec = env.new_object(
                jni_str!("javax/crypto/spec/GCMParameterSpec"),
                jni_sig!("(I[B)V"),
                &[JValue::Int(128), JValue::Object(&iv)],
            )?;
            let cipher = aes_gcm(env)?;
            env.call_method(
                &cipher,
                jni_str!("init"),
                jni_sig!("(ILjava/security/Key;Ljava/security/spec/AlgorithmParameterSpec;)V"),
                // Cipher.DECRYPT_MODE
                &[
                    JValue::Int(2),
                    JValue::Object(&secret),
                    JValue::Object(&spec),
                ],
            )?;
            do_final(env, &cipher, sealed).map(Some)
        })
        .map_err(keystore_error)?;
        match plain {
            None => Ok(None),
            Some(plain) => plain
                .try_into()
                .map(Some)
                .map_err(|_| SecretsError::Corrupt),
        }
    }
}

fn keystore_error(err: AndroidError) -> SecretsError {
    SecretsError::Platform(format!("Android Keystore: {err}"))
}

/// Proxy e autorità di certificazione di Android sul client HTTP. Il proxy di sistema (impostazioni del Wi-Fi)
/// Android lo mette nelle proprietà di Java `http(s).proxyHost`, che reqwest non legge: lui
/// guarda solo le variabili d'ambiente, che le app Android non hanno. Il proxy si legge alla
/// creazione del client; un proxy automatico (PAC) non è gestito.
pub(crate) fn configure_http(mut builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let no_proxy = system_property("http.nonProxyHosts")
        .ok()
        .flatten()
        .map(|hosts| no_proxy_list(&hosts));
    for scheme in ["http", "https"] {
        let host = system_property(&format!("{scheme}.proxyHost"))
            .ok()
            .flatten();
        let port = system_property(&format!("{scheme}.proxyPort"))
            .ok()
            .flatten();
        let Some(url) = host.and_then(|h| proxy_url(&h, port.as_deref())) else {
            continue;
        };
        let proxy = match scheme {
            "http" => reqwest::Proxy::http(&url),
            _ => reqwest::Proxy::https(&url),
        };
        match proxy {
            Ok(proxy) => {
                log::info!("proxy di sistema per {scheme}: {url}");
                let no_proxy = no_proxy.as_deref().and_then(reqwest::NoProxy::from_string);
                builder = builder.proxy(proxy.no_proxy(no_proxy));
            }
            Err(err) => log::warn!("proxy di sistema non valido ({url}): {err}"),
        }
    }
    // le autorità di sistema lette dai file, non il gestore dei certificati di Android
    // (rustls-platform-verifier): quello vuole un componente Kotlin nell'APK e controlla le revoche
    // scaricando le liste in chiaro, fino a 30 s per connessione su una rete con l'IPv6 rotto
    // (docs/m5-fase1.md). Così non si vedono le revoche né le autorità aggiunte dall'utente.
    let certs = system_certificates();
    if certs.is_empty() {
        log::error!("nessuna autorità di certificazione di sistema: l'HTTPS non funzionerà");
    }
    builder.tls_certs_only(certs)
}

/// L'indirizzo del proxy da `proxyHost` e `proxyPort` (80 se manca, come fa Java).
fn proxy_url(host: &str, port: Option<&str>) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || host.contains(['/', '@', ' ']) {
        return None;
    }
    let port = match port.map(str::trim).filter(|p| !p.is_empty()) {
        Some(port) => port.parse::<u16>().ok()?,
        None => 80,
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    Some(format!("http://{host}:{port}"))
}

/// `http.nonProxyHosts` di Java (`localhost|*.esempio.it`) nel formato di reqwest
/// (`localhost,.esempio.it`). reqwest accetta solo `*` da solo come jolly: `*.dominio` diventa
/// `.dominio` (stesso significato), gli altri jolly (`192.168.*`) si scartano.
fn no_proxy_list(java: &str) -> String {
    java.split('|')
        .map(str::trim)
        .filter_map(|entry| match entry.strip_prefix("*.") {
            Some(domain) if !domain.contains('*') => Some(format!(".{domain}")),
            _ if entry == "*" || (!entry.is_empty() && !entry.contains('*')) => {
                Some(entry.to_owned())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Le autorità di certificazione di sistema dai file di Android (un PEM per file, seguito dalla
/// descrizione testuale del certificato).
fn system_certificates() -> Vec<reqwest::Certificate> {
    let Some(dir) = SYSTEM_CA_DIRS
        .iter()
        .map(std::path::Path::new)
        .find(|p| p.is_dir())
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let certs: Vec<_> = entries
        .filter_map(|entry| std::fs::read(entry.ok()?.path()).ok())
        .filter_map(|pem| reqwest::Certificate::from_pem_bundle(&pem).ok())
        .flatten()
        .collect();
    log::info!(
        "{} autorità di certificazione da {}",
        certs.len(),
        dir.display()
    );
    certs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_url_from_java_properties() {
        assert_eq!(
            proxy_url("10.0.0.1", Some("3128")).as_deref(),
            Some("http://10.0.0.1:3128")
        );
        assert_eq!(
            proxy_url("proxy.lan", None).as_deref(),
            Some("http://proxy.lan:80")
        );
        assert_eq!(
            proxy_url("fe80::1", Some("8080")).as_deref(),
            Some("http://[fe80::1]:8080")
        );
        assert_eq!(proxy_url(" ", Some("8080")), None);
        assert_eq!(proxy_url("utente@host", Some("8080")), None);
        assert_eq!(proxy_url("host", Some("porta")), None);
    }

    #[test]
    fn java_non_proxy_hosts_become_reqwest_no_proxy() {
        assert_eq!(
            no_proxy_list("localhost|*.esempio.it| 192.168.1.5 |10.*|"),
            "localhost,.esempio.it,192.168.1.5"
        );
        assert_eq!(no_proxy_list("*"), "*");
    }

    #[test]
    fn system_certificates_are_readable() {
        // nella shell di adb come nell'app: le autorità di sistema sono leggibili da tutti
        assert!(system_certificates().len() > 50);
    }

    #[test]
    fn calls_before_init_fail_cleanly() {
        if RUNTIME.get().is_none() {
            assert!(matches!(app_dirs(), Err(AndroidError::NotInitialized)));
            assert!(system_property("http.proxyHost").is_err());
        }
    }
}
