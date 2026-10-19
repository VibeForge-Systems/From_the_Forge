//! Writes the viewer media catalogue, `tests/fixtures/viewer/media/`
//! (M5 spec §14.1): tiny images from the `image` crate's encoders,
//! hand-written hostile PNGs, minimal and hostile containers, and hostile
//! text files. See `media.rs` for what each file is.
//!
//! ```sh
//! cargo run -p oma-archive --example make_viewer_media -- <out-dir>
//! ```
//!
//! It refuses to write into a directory that already exists. The committed
//! files are pinned by `tests/fixtures/viewer/SHA256SUMS`, and
//! `tests/viewer_fixtures.rs` rebuilds every file with `media.rs` and
//! compares the bytes, so a change to this code or to the pinned `image`
//! version shows up as a test failure. Synthetic content only.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod media;

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(out) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: make_viewer_media <out-dir>");
        return ExitCode::from(2);
    };
    if out.exists() {
        eprintln!("refusing to write into an existing path");
        return ExitCode::from(1);
    }
    fs::create_dir_all(&out).expect("create the output directory");
    for f in media::all() {
        fs::write(out.join(f.name), &f.bytes).expect("write");
        println!("{:>9}  {}", f.bytes.len(), f.name);
    }
    ExitCode::SUCCESS
}
