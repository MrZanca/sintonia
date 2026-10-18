//! Ogni testo `@tr(...)` dell'interfaccia deve avere la traduzione italiana, e viceversa.

use std::collections::BTreeSet;
use std::path::Path;

/// Estrae le stringhe (singolari) di `@tr("…")` e `@tr("…" | "…" % n)`.
fn slint_strings(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = source;
    while let Some(at) = rest.find("@tr(\"") {
        rest = &rest[at + 5..];
        let mut text = String::new();
        let mut chars = rest.char_indices();
        let mut end = rest.len();
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => {
                    if let Some((_, escaped)) = chars.next() {
                        text.push(escaped);
                    }
                }
                '"' => {
                    end = i + 1;
                    break;
                }
                c => text.push(c),
            }
        }
        found.insert(text);
        rest = &rest[end..];
    }
    found
}

/// Le chiavi `msgid` (non vuote) di un file `.po`.
fn po_ids(po: &str) -> BTreeSet<String> {
    po.lines()
        .filter_map(|l| l.strip_prefix("msgid \""))
        .filter_map(|l| l.strip_suffix('"'))
        .filter(|id| !id.is_empty())
        .map(|id| id.replace("\\\"", "\""))
        .collect()
}

#[test]
fn italian_translation_is_complete() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let slint = std::fs::read_to_string(root.join("ui/app.slint")).expect("app.slint");
    let po =
        std::fs::read_to_string(root.join("lang/it/LC_MESSAGES/sintonia-app.po")).expect("it.po");
    let used = slint_strings(&slint);
    let translated = po_ids(&po);
    let missing: Vec<_> = used.difference(&translated).collect();
    let unused: Vec<_> = translated.difference(&used).collect();
    assert!(missing.is_empty(), "senza traduzione: {missing:?}");
    assert!(unused.is_empty(), "traduzioni non più usate: {unused:?}");
}
