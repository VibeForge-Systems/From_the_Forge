//! `src/types/generated/export-words.json` (M6 spec §4.5, D129): written
//! from `oma_export::words` like the ts-rs bindings, so the generated-files
//! drift check (AGENTS.md, "Before opening a PR", step 7) keeps it
//! committed, and the Vitest `export-words.spec.ts` compares it with
//! `viewerText.ts`.

use std::path::PathBuf;

fn generated_dir() -> PathBuf {
    // `.cargo/config.toml` sets TS_RS_EXPORT_DIR for every build.
    match option_env!("TS_RS_EXPORT_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("src")
            .join("types")
            .join("generated"),
    }
}

#[test]
fn export_words_json_is_written_and_stable() {
    let text = oma_export::words::words_json();
    assert!(!text.contains('\r'), "LF line endings only");
    assert_eq!(text, oma_export::words::words_json(), "deterministic");
    let path = generated_dir().join("export-words.json");
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    // Git for Windows may check the file out with CRLF; the content is
    // what is compared.
    if current.replace("\r\n", "\n") != text {
        std::fs::write(&path, &text).expect("write export-words.json");
    }
    let doc: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(doc["format"], "oma-export-words");
    assert!(doc["tables"].as_array().expect("tables").len() >= 39);
}
