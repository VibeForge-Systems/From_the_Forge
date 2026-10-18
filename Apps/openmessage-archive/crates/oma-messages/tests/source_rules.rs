//! Rules on the crate's own sources (M3 §5.2, §15).
//!
//! - No `#[tracing::instrument]` without `skip_all`: it would format every
//!   argument with `Debug`.
//! - No `get::<` on rows: values are read as `ValueRef` and classified, so
//!   a storage class never converts silently.
use std::fs;
use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).expect("read_dir").flatten() {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn source_rules() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() >= 10);
    for f in files {
        let text = fs::read_to_string(&f).expect("read");
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            let at = format!("{}:{}", f.display(), n + 1);
            assert!(!code.contains("get::<"), "{at}: use ValueRef readers");
            if code.contains("instrument") {
                assert!(code.contains("skip_all"), "{at}: instrument needs skip_all");
            }
        }
    }
}
