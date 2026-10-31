//! Le autorità di certificazione per l'HTTPS di mpv.
//!
//! mpv verifica i certificati dei server (`tls-verify`). Su Windows libcurl usa quelli di
//! sistema; su Android libcurl e FFmpeg usano mbedtls, che non sa dove Android tiene i suoi: senza
//! un file di autorità ogni flusso `https` fallisce con «TLS certificate verification failed».
//! Si raccolgono quindi i certificati di sistema in un file PEM nella cache dell'app.

/// I blocchi `CERTIFICATE` di un testo PEM, intestazioni comprese. I file di Android hanno dopo il
/// blocco la descrizione testuale del certificato, che a mbedtls non serve.
pub fn pem_certificates(text: &str) -> impl Iterator<Item = &str> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let mut rest = text;
    std::iter::from_fn(move || {
        let start = rest.find(BEGIN)?;
        let end = start + rest[start..].find(END)? + END.len();
        let block = &rest[start..end];
        rest = &rest[end..];
        Some(block)
    })
}

/// Scrive in `dir/cacerts.pem` le autorità di sistema di Android e ne restituisce il percorso.
/// Da Android 14 l'elenco aggiornabile sta nel modulo Conscrypt; prima in `/system`.
#[cfg(target_os = "android")]
pub fn system_ca_bundle(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;

    let source = [
        "/apex/com.android.conscrypt/cacerts",
        "/system/etc/security/cacerts",
    ]
    .into_iter()
    .map(std::path::Path::new)
    .find(|p| p.is_dir())
    .ok_or_else(|| std::io::Error::other("cartella dei certificati di sistema non trovata"))?;
    let mut bundle = String::new();
    let mut count = 0usize;
    for entry in std::fs::read_dir(source)? {
        // un file illeggibile non deve far perdere tutti gli altri
        let Ok(text) = entry.and_then(|e| std::fs::read_to_string(e.path())) else {
            continue;
        };
        for block in pem_certificates(&text) {
            bundle.push_str(block);
            bundle.push('\n');
            count += 1;
        }
    }
    if count == 0 {
        return Err(std::io::Error::other(
            "nessun certificato di sistema leggibile",
        ));
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join("cacerts.pem");
    // scritto a parte e poi rinominato: mpv non deve mai leggere un file a metà
    let partial = dir.join("cacerts.pem.tmp");
    std::fs::File::create(&partial)?.write_all(bundle.as_bytes())?;
    std::fs::rename(&partial, &path)?;
    log::info!("{count} autorità di certificazione da {}", source.display());
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::pem_certificates;

    #[test]
    fn keeps_only_certificate_blocks() {
        let text = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n\
                    Certificate:\n    Data: ...\n\
                    -----BEGIN CERTIFICATE-----\nBBBB\n-----END CERTIFICATE-----\nSHA1 Fingerprint";
        let blocks: Vec<_> = pem_certificates(text).collect();
        assert_eq!(
            blocks,
            [
                "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----",
                "-----BEGIN CERTIFICATE-----\nBBBB\n-----END CERTIFICATE-----",
            ]
        );
    }

    #[test]
    fn ignores_text_without_certificates_or_truncated() {
        assert_eq!(pem_certificates("niente").count(), 0);
        assert_eq!(
            pem_certificates("-----BEGIN CERTIFICATE-----\nAAAA").count(),
            0
        );
    }
}
