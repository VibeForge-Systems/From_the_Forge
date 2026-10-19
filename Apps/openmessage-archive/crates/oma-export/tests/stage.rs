//! Staging, publish, provenance and leftovers (M6 spec §7, §8.3–§8.6,
//! §14.2 "Manifest", "Destination and placement", "Sweep"; slice 6e1).
//!
//! Every test writes into `<tmp>/dest` and checks that nothing appears
//! beside it. The Linux-only tests are the swap tests (D143) and the mode
//! checks (D140); the rest run on every system.
#![allow(clippy::disallowed_methods)]

mod files;

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use files::*;
use oma_export::manifest::write_provenance;
use oma_export::map::ExportZone;
use oma_export::naming::{BundlePath, EntryKind, EntryName, FileExt, FileStem, TargetName, MARKER};
use oma_export::plan::Format;
use oma_export::stage::{
    find_leftovers, probe_writable, remove_leftovers, retry_with, Replace, Staging,
    RETRY_BACKOFF_MS, SWEEP_MIN_AGE,
};
use oma_export::{ExportError, LimitKind};

fn txt(n: u64) -> BundlePath {
    BundlePath::format_file(Format::Txt, FileStem::Conversation(n), FileExt::Txt)
}

fn write(s: &mut Staging, p: &BundlePath, bytes: &[u8]) {
    let mut f = s.create(p).expect("create");
    f.write_all(bytes).expect("write");
    s.close(f).expect("close");
}

fn name(s: &str) -> TargetName {
    TargetName::new(s).expect("name")
}

/// Only `dest` exists beside the destination's contents.
fn nothing_beside(d: &Dest) {
    let top: Vec<String> = std::fs::read_dir(d.tmp.path())
        .expect("tmp")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(top, ["dest"]);
}

#[test]
fn a_bundle_publishes_whole_with_provenance_last_and_private_modes() {
    let d = dest();
    let plan = plan(false);
    let mut s = Staging::bundle(&d.parent).expect("staging");
    let staging_name = s.entry().to_name();
    assert!(s.marker_locked());
    write(&mut s, &txt(1), b"one\n");
    write(&mut s, &txt(2), b"two\n");
    write(
        &mut s,
        &BundlePath::format_file(Format::Txt, FileStem::NoConversation, FileExt::Txt),
        b"none\n",
    );
    // Before publish only the staging folder exists, with its marker.
    assert_eq!(
        tree(&d.parent),
        [
            format!("{staging_name}/"),
            format!("{staging_name}/{MARKER}"),
            format!("{staging_name}/txt/"),
            format!("{staging_name}/txt/c0001.txt"),
            format!("{staging_name}/txt/c0002.txt"),
            format!("{staging_name}/txt/no-conversation.txt"),
        ]
    );
    let r = write_provenance(&mut s, &plan, &body(&plan, 2)).expect("provenance");
    assert_eq!((r.files_listed, r.sums_lines), (4, 5));
    // Write order: content, README, manifest, sums.
    let order: Vec<String> = s.files().iter().map(|f| f.path().to_slash()).collect();
    assert_eq!(
        order[3..],
        ["README.txt", "manifest.json", "SHA256SUMS"].map(String::from)
    );
    let p = s
        .publish(&name("Messages export"), Replace::Never)
        .expect("publish");
    assert_eq!(
        (p.candidate, p.marker_removed, p.replaced),
        (1, true, false)
    );
    let bundle = d.parent.join("Messages export");
    assert_eq!(
        tree(&d.parent),
        [
            "Messages export/",
            "Messages export/README.txt",
            "Messages export/SHA256SUMS",
            "Messages export/manifest.json",
            "Messages export/txt/",
            "Messages export/txt/c0001.txt",
            "Messages export/txt/c0002.txt",
            "Messages export/txt/no-conversation.txt",
        ]
    );
    nothing_beside(&d);
    // SHA256SUMS covers every file, manifest.json included.
    let sums = verify_sums(&bundle);
    assert_eq!(
        sums.keys().cloned().collect::<Vec<_>>(),
        [
            "README.txt",
            "manifest.json",
            "txt/c0001.txt",
            "txt/c0002.txt",
            "txt/no-conversation.txt"
        ]
    );
    // manifest.json lists every file but itself and the sums, with the
    // hashes the stream computed.
    let m: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle.join("manifest.json")).expect("read"))
            .expect("json");
    let listed: Vec<(String, String)> = m["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| {
            (
                f["path"].as_str().expect("path").to_owned(),
                f["sha256"].as_str().expect("sha").to_owned(),
            )
        })
        .collect();
    assert_eq!(listed.len(), 4);
    for (path, sha) in &listed {
        assert_eq!(sums.get(path), Some(sha), "{path}");
    }
    assert_eq!(
        m["files"][0]["bytes"],
        serde_json::json!(std::fs::metadata(bundle.join("README.txt"))
            .expect("m")
            .len())
    );
    assert_eq!(m["exportId"], "07070707070707070707070707070707");
    assert_eq!(
        m["scope"],
        serde_json::json!({"kind": "archive", "conversations": 2})
    );
    assert_eq!(m["formats"], serde_json::json!(["txt"]));
    assert_eq!(m["createdUtc"], "2027-01-01T00:00:00Z");
    assert_eq!(m["zone"]["name"], "UTC");
    assert_eq!(m["search"], serde_json::Value::Null);
    assert_eq!(
        m["exclusions"],
        serde_json::json!([
            "d94",
            "d115Raw",
            "rawSourceBytes",
            "sourceIdentifierOptIn",
            "sessionLocalRowid",
            "storageInternal",
            "recentlyDeletedOnlyNeverImported",
            "attachmentsNotConverted"
        ])
    );
    let readme = std::fs::read_to_string(bundle.join("README.txt")).expect("readme");
    assert!(readme.starts_with("OpenMessage Archive export\n\nThis folder is an export"));
    assert!(!readme.contains('\r'));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&bundle), 0o700);
        assert_eq!(mode(&bundle.join("txt")), 0o700);
        for f in ["README.txt", "manifest.json", "SHA256SUMS", "txt/c0001.txt"] {
            let p = f.split('/').fold(bundle.clone(), |p, c| p.join(c));
            assert_eq!(mode(&p), 0o600, "{f}");
        }
    }
    // A sweep never touches a published bundle.
    let scan = find_leftovers(&d.parent, Duration::ZERO).expect("scan");
    assert!(scan.found.is_empty());
}

#[test]
fn a_taken_name_gets_the_next_suffix_and_nothing_is_ever_replaced() {
    let d = dest();
    std::fs::create_dir(d.parent.join("E")).expect("mkdir");
    std::fs::write(d.parent.join("E (2)"), b"keep").expect("file");
    let mut s = Staging::bundle(&d.parent).expect("staging");
    write(&mut s, &txt(1), b"x");
    let p = s.publish(&name("E"), Replace::Never).expect("publish");
    assert_eq!(p.candidate, 3);
    assert_eq!(p.name.as_str(), "E (3)");
    assert_eq!(
        std::fs::read(d.parent.join("E (2)")).expect("kept"),
        b"keep"
    );
    assert!(d
        .parent
        .join("E (3)")
        .join("txt")
        .join("c0001.txt")
        .is_file());
    // A bundle can never replace.
    let mut s = Staging::bundle(&d.parent).expect("staging");
    assert!(matches!(
        s.publish(&name("F"), Replace::ConfirmedRegularFile),
        Err(ExportError::Limit(LimitKind::RowOrder))
    ));
    assert!(s.remove());
}

#[test]
fn ninety_nine_taken_names_fail_with_target_exists_and_the_staging_is_removed() {
    let d = dest();
    let base = name("Full");
    for n in 1..=99 {
        std::fs::create_dir(d.parent.join(base.candidate(n).as_str())).expect("mkdir");
    }
    let mut s = Staging::bundle(&d.parent).expect("staging");
    write(&mut s, &txt(1), b"x");
    assert!(matches!(
        s.publish(&base, Replace::Never),
        Err(ExportError::TargetExists)
    ));
    assert!(s.remove());
    assert_eq!(tree(&d.parent).len(), 99);
    nothing_beside(&d);
}

#[test]
fn cancel_removes_exactly_what_was_created_and_drop_cleans_up_too() {
    let d = dest();
    let mut s = Staging::bundle(&d.parent).expect("staging");
    write(&mut s, &txt(1), b"x");
    let open = s.create(&txt(2)).expect("open file");
    assert!(s.remove(), "staging removed");
    drop(open);
    assert!(tree(&d.parent).is_empty());
    // A second remove reports the same outcome and does nothing.
    assert!(s.remove());
    // Writing after removal is refused.
    assert!(matches!(
        s.create(&txt(3)),
        Err(ExportError::Limit(LimitKind::RowOrder))
    ));

    // Dropped without publish or remove: removed too.
    {
        let mut s = Staging::bundle(&d.parent).expect("staging");
        write(&mut s, &txt(1), b"x");
    }
    assert!(tree(&d.parent).is_empty());

    // Something the export did not create keeps the folder (and is kept).
    let mut s = Staging::bundle(&d.parent).expect("staging");
    let staging = d.parent.join(s.entry().to_name());
    std::fs::write(staging.join("foreign"), b"not ours").expect("plant");
    assert!(!s.remove());
    assert_eq!(
        tree(&d.parent),
        [
            format!("{}/", s.entry().to_name()),
            format!("{}/foreign", s.entry().to_name()),
        ]
    );
    nothing_beside(&d);
}

#[test]
fn files_must_be_closed_before_provenance_and_publish() {
    let d = dest();
    let plan = plan(false);
    let mut s = Staging::bundle(&d.parent).expect("staging");
    let f = s.create(&txt(1)).expect("create");
    assert!(matches!(
        write_provenance(&mut s, &plan, &body(&plan, 1)),
        Err(ExportError::Limit(LimitKind::FileStillOpen))
    ));
    assert!(matches!(
        s.publish(&name("x"), Replace::Never),
        Err(ExportError::Limit(LimitKind::FileStillOpen))
    ));
    s.close(f).expect("close");
    // The same path twice, or one differing only by case, is refused.
    assert!(matches!(
        s.create(&txt(1)),
        Err(ExportError::Limit(LimitKind::NameCollision))
    ));
    // Counts that break an identity stop provenance before README.
    let mut bad = body(&plan, 1);
    bad.counts.items = 1;
    assert!(matches!(
        write_provenance(&mut s, &plan, &bad),
        Err(ExportError::Limit(LimitKind::RowCountMismatch))
    ));
    assert!(!s
        .files()
        .iter()
        .any(|f| f.path().to_slash() == "README.txt"));
    assert!(s.remove());
}

#[test]
fn single_files_are_placed_without_replacing_unless_confirmed() {
    let d = dest();
    let one = |content: &[u8]| {
        let mut s = Staging::single_file(&d.parent).expect("staging");
        let mut f = s.create(&txt(1)).expect("create");
        f.write_all(content).expect("write");
        s.close(f).expect("close");
        // One output only.
        assert!(matches!(
            s.create(&txt(2)),
            Err(ExportError::Limit(LimitKind::RowOrder))
        ));
        s
    };
    let target = TargetName::for_single_file("chat.html", Format::Txt).expect("name");
    assert_eq!(target.as_str(), "chat.html.txt");
    let mut s = one(b"first");
    let p = s.publish(&target, Replace::Never).expect("publish");
    assert!(!p.replaced);
    assert_eq!(
        std::fs::read(d.parent.join("chat.html.txt")).expect("r"),
        b"first"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(d.parent.join("chat.html.txt"))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Exists: refused without confirmation, and the partial is removed.
    let mut s = one(b"second");
    assert!(matches!(
        s.publish(&target, Replace::Never),
        Err(ExportError::TargetExists)
    ));
    assert!(s.remove());
    assert_eq!(
        std::fs::read(d.parent.join("chat.html.txt")).expect("r"),
        b"first"
    );
    // Confirmed: a regular file is replaced.
    let mut s = one(b"third");
    let p = s
        .publish(&target, Replace::ConfirmedRegularFile)
        .expect("replace");
    assert!(p.replaced);
    assert_eq!(
        std::fs::read(d.parent.join("chat.html.txt")).expect("r"),
        b"third"
    );
    // A folder (or a link) is never replaced, even when confirmed.
    std::fs::create_dir(d.parent.join("dir.txt")).expect("mkdir");
    let mut s = one(b"fourth");
    let t = TargetName::new("dir.txt").expect("name");
    assert!(matches!(
        s.publish(&t, Replace::ConfirmedRegularFile),
        Err(ExportError::TargetExists)
    ));
    assert!(s.remove());
    #[cfg(unix)]
    {
        let outside = d.tmp.path().join("outside.txt");
        std::fs::write(&outside, b"keep").expect("outside");
        std::os::unix::fs::symlink(&outside, d.parent.join("link.txt")).expect("symlink");
        let mut s = one(b"fifth");
        let t = TargetName::new("link.txt").expect("name");
        assert!(matches!(
            s.publish(&t, Replace::ConfirmedRegularFile),
            Err(ExportError::TargetExists)
        ));
        assert!(s.remove());
        assert_eq!(std::fs::read(&outside).expect("r"), b"keep");
        std::fs::remove_file(d.parent.join("link.txt")).expect("unlink");
        std::fs::remove_file(&outside).expect("rm");
    }
    assert_eq!(tree(&d.parent), ["chat.html.txt", "dir.txt/"]);
    nothing_beside(&d);
}

#[test]
fn the_destination_must_be_canonical() {
    let d = dest();
    let not_canonical = d.parent.join("..").join("dest");
    assert!(matches!(
        Staging::bundle(&not_canonical),
        Err(ExportError::DestinationChanged)
    ));
    assert!(matches!(
        Staging::bundle(&d.parent.join("missing")),
        Err(ExportError::DestinationChanged)
    ));
    #[cfg(unix)]
    {
        let link = d.tmp.path().join("link");
        std::os::unix::fs::symlink(&d.parent, &link).expect("symlink");
        assert!(matches!(
            Staging::bundle(&link),
            Err(ExportError::DestinationChanged)
        ));
        std::fs::remove_file(&link).expect("unlink");
    }
    assert!(tree(&d.parent).is_empty());
}

/// D143, spec §14.2: another account renames the staging folder in a
/// shared parent and plants a link to somewhere else (in the attack, a
/// backup) under its name, mid-export. No byte lands outside the staging
/// folder the export made, and the publish fails on the `(dev, ino)` check.
#[cfg(target_os = "linux")]
#[test]
fn a_swapped_staging_folder_cannot_redirect_writes_or_publish() {
    let d = dest();
    let outside = d.tmp.path().join("outside");
    std::fs::create_dir(&outside).expect("outside");
    let mut s = Staging::bundle(&d.parent).expect("staging");
    let staging_name = s.entry().to_name();
    write(&mut s, &txt(1), b"before");
    // The swap.
    let moved = d.parent.join("moved");
    std::fs::rename(d.parent.join(&staging_name), &moved).expect("rename");
    std::os::unix::fs::symlink(&outside, d.parent.join(&staging_name)).expect("plant");
    // Writing continues, into a new folder as well.
    write(&mut s, &txt(2), b"after");
    write(
        &mut s,
        &BundlePath::format_file(Format::Csv, FileStem::NoConversation, FileExt::Csv),
        b"after",
    );
    assert!(tree(&outside).is_empty(), "nothing outside staging");
    assert_eq!(
        tree(&moved),
        [
            MARKER,
            "csv/",
            "csv/no-conversation.csv",
            "txt/",
            "txt/c0001.txt",
            "txt/c0002.txt"
        ]
    );
    assert!(matches!(
        s.publish(&name("Swapped"), Replace::Never),
        Err(ExportError::DestinationChanged)
    ));
    assert!(!d.parent.join("Swapped").exists());
    // Removal removes our files through the handles, and refuses to
    // remove anything by the swapped name.
    assert!(!s.remove());
    assert!(tree(&outside).is_empty());
    assert!(tree(&moved).is_empty());
    assert!(std::fs::symlink_metadata(d.parent.join(&staging_name))
        .expect("link kept")
        .file_type()
        .is_symlink());
}

/// The parent itself is moved and replaced by a link after staging began:
/// the publish refuses rather than place the bundle elsewhere.
#[cfg(target_os = "linux")]
#[test]
fn a_swapped_parent_fails_the_publish() {
    let d = dest();
    let mut s = Staging::bundle(&d.parent).expect("staging");
    write(&mut s, &txt(1), b"x");
    let moved = d.tmp.path().join("moved-dest");
    std::fs::rename(&d.parent, &moved).expect("move parent");
    let elsewhere = d.tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("mkdir");
    std::os::unix::fs::symlink(&elsewhere, &d.parent).expect("plant");
    assert!(matches!(
        s.publish(&name("Moved"), Replace::Never),
        Err(ExportError::DestinationChanged)
    ));
    assert!(tree(&elsewhere).is_empty());
    // Removal empties the real staging folder through its handle but
    // does not remove by name through the link.
    assert!(!s.remove());
    assert!(tree(&elsewhere).is_empty());
    let left = tree(&moved);
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].starts_with(".oma-export-") && left[0].ends_with(".partial/"));
}

fn leftover_dir(parent: &Path, id: u8, marker: bool, contents: bool) -> String {
    let n = EntryName::new(
        oma_export::EntryId::from_bytes([id; 16]),
        EntryKind::PartialDir,
    )
    .to_name();
    let p = parent.join(&n);
    std::fs::create_dir(&p).expect("mkdir");
    if marker {
        std::fs::write(p.join(MARKER), b"m").expect("marker");
    }
    if contents {
        std::fs::create_dir(p.join("txt")).expect("sub");
        std::fs::write(p.join("txt").join("c0001.txt"), b"plaintext").expect("file");
    }
    n
}

fn leftover_file(parent: &Path, id: u8, kind: EntryKind) -> String {
    let n = EntryName::new(oma_export::EntryId::from_bytes([id; 16]), kind).to_name();
    std::fs::write(parent.join(&n), b"partial").expect("file");
    n
}

#[test]
fn the_sweep_finds_only_dead_leftovers_and_removes_exactly_the_pinned_list() {
    let d = dest();
    let marked = leftover_dir(&d.parent, 1, true, true);
    let empty = leftover_dir(&d.parent, 2, false, false);
    let unmarked_full = leftover_dir(&d.parent, 3, false, true);
    let partial_file = leftover_file(&d.parent, 4, EntryKind::PartialFile);
    let probe = leftover_file(&d.parent, 5, EntryKind::Probe);
    // Names the pattern does not cover are never touched.
    std::fs::create_dir(d.parent.join(".oma-export-partial")).expect("mkdir");
    std::fs::write(d.parent.join("Messages export"), b"real").expect("file");
    std::fs::write(
        d.parent
            .join(".oma-export-0000000000000000000000000000000Z.partial-file"),
        b"x",
    )
    .expect("file");
    // A file named like a folder leftover, and a folder named like a file.
    std::fs::write(
        d.parent.join(
            EntryName::new(
                oma_export::EntryId::from_bytes([6; 16]),
                EntryKind::PartialDir,
            )
            .to_name(),
        ),
        b"x",
    )
    .expect("file");
    std::fs::create_dir(d.parent.join(
        EntryName::new(oma_export::EntryId::from_bytes([7; 16]), EntryKind::Probe).to_name(),
    ))
    .expect("dir");
    // A live export of this process: its marker is locked.
    let mut live = Staging::bundle(&d.parent).expect("live");
    write(&mut live, &txt(1), b"live");
    let mut live_file = Staging::single_file(&d.parent).expect("live single");
    let f = live_file.create(&txt(1)).expect("create");

    // Everything is younger than five minutes.
    let young = find_leftovers(&d.parent, SWEEP_MIN_AGE).expect("scan");
    assert!(young.found.is_empty());
    assert_eq!(young.skipped_recent, 4);
    assert_eq!(young.skipped_locked, 2);

    let scan = find_leftovers(&d.parent, Duration::ZERO).expect("scan");
    let found: Vec<(String, bool)> = scan
        .found
        .iter()
        .map(|l| (l.name.to_name(), l.has_marker))
        .collect();
    let mut want = vec![
        (marked.clone(), true),
        (empty.clone(), false),
        (partial_file.clone(), false),
        (probe.clone(), false),
    ];
    want.sort();
    let mut found_sorted = found.clone();
    found_sorted.sort();
    assert_eq!(found_sorted, want);
    assert_eq!(scan.skipped_locked, 2, "the live exports");
    assert_eq!(scan.skipped_other, 3, "unmarked folder, wrong types");

    // A leftover that appears after the scan is not removed.
    let late = leftover_dir(&d.parent, 9, true, false);
    let report = remove_leftovers(&d.parent, &scan.found);
    assert_eq!((report.removed, report.skipped, report.failed), (4, 0, 0));
    for gone in [&marked, &empty, &partial_file, &probe] {
        assert!(
            std::fs::symlink_metadata(d.parent.join(gone)).is_err(),
            "{gone}"
        );
    }
    for kept in [&unmarked_full, &late] {
        assert!(d.parent.join(kept).is_dir(), "{kept}");
    }
    assert_eq!(
        std::fs::read(d.parent.join("Messages export")).expect("r"),
        b"real"
    );
    // The live exports survived and still publish.
    live_file.close(f).expect("close");
    live_file
        .publish(&name("live.txt"), Replace::Never)
        .expect("live single publishes");
    live.publish(&name("Live"), Replace::Never)
        .expect("live publishes");
    nothing_beside(&d);
}

#[test]
fn a_leftover_that_changed_after_the_scan_is_skipped() {
    let d = dest();
    let marked = leftover_dir(&d.parent, 1, true, true);
    let pf = leftover_file(&d.parent, 2, EntryKind::PartialFile);
    let unmarked = leftover_dir(&d.parent, 3, true, true);
    let scan = find_leftovers(&d.parent, Duration::ZERO).expect("scan");
    assert_eq!(scan.found.len(), 3);
    // Replaced by entries of the same name: a folder that lost its
    // marker, and a folder where the partial file was.
    std::fs::remove_dir_all(d.parent.join(&unmarked)).expect("rm");
    leftover_dir(&d.parent, 3, false, true);
    std::fs::remove_file(d.parent.join(&pf)).expect("rm");
    std::fs::create_dir(d.parent.join(&pf)).expect("dir now");
    // A marked folder replaced by another marked folder of the same name:
    // only the `(dev, ino)` re-check tells them apart, which exists on
    // Linux only (Windows re-checks by name, type, marker and lock: the
    // disclosed residual).
    std::fs::rename(d.parent.join(&marked), d.tmp.path().join("old")).expect("move");
    leftover_dir(&d.parent, 1, true, true);
    let report = remove_leftovers(&d.parent, &scan.found);
    assert!(d
        .parent
        .join(&unmarked)
        .join("txt")
        .join("c0001.txt")
        .is_file());
    assert!(d.parent.join(&pf).is_dir());
    if cfg!(target_os = "linux") {
        assert_eq!((report.removed, report.skipped), (0, 3));
        assert!(d
            .parent
            .join(&marked)
            .join("txt")
            .join("c0001.txt")
            .is_file());
    } else {
        assert_eq!((report.removed, report.skipped), (1, 2));
    }
}

/// On Linux a planted link inside a leftover is removed as a link: the
/// target outside is untouched.
#[cfg(unix)]
#[test]
fn removing_a_leftover_never_follows_a_link_inside_it() {
    let d = dest();
    let outside = d.tmp.path().join("outside");
    std::fs::create_dir(&outside).expect("mkdir");
    std::fs::write(outside.join("precious"), b"keep").expect("file");
    let marked = leftover_dir(&d.parent, 1, true, true);
    std::os::unix::fs::symlink(&outside, d.parent.join(&marked).join("txt").join("link"))
        .expect("plant");
    let scan = find_leftovers(&d.parent, Duration::ZERO).expect("scan");
    let report = remove_leftovers(&d.parent, &scan.found);
    assert_eq!(report.removed, 1);
    assert_eq!(
        std::fs::read(outside.join("precious")).expect("kept"),
        b"keep"
    );
    assert!(tree(&d.parent).is_empty());
}

#[test]
fn the_probe_leaves_nothing_behind() {
    let d = dest();
    assert!(probe_writable(&d.parent).expect("probe"));
    assert!(tree(&d.parent).is_empty());
}

#[test]
fn retries_back_off_boundedly_and_only_for_transient_errors() {
    let total: u64 = RETRY_BACKOFF_MS.iter().sum();
    assert!(total <= 10_000);
    assert_eq!(RETRY_BACKOFF_MS[0], 100);
    assert!(RETRY_BACKOFF_MS.iter().all(|ms| *ms <= 2_000));
    let mut slept = Vec::new();
    let mut calls = 0;
    let r: std::io::Result<()> = retry_with(
        &mut |d| slept.push(d),
        &|e| e.kind() == std::io::ErrorKind::PermissionDenied,
        || {
            calls += 1;
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        },
    );
    assert!(r.is_err());
    assert_eq!(calls, RETRY_BACKOFF_MS.len() + 1);
    assert_eq!(
        slept,
        RETRY_BACKOFF_MS
            .iter()
            .map(|ms| Duration::from_millis(*ms))
            .collect::<Vec<_>>()
    );
    // Success after two transient failures; a permanent error at once.
    let mut n = 0;
    let r = retry_with(
        &mut |_| {},
        &|e| e.kind() == std::io::ErrorKind::PermissionDenied,
        || {
            n += 1;
            if n < 3 {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                Ok(n)
            }
        },
    );
    assert_eq!(r.expect("ok"), 3);
    let mut n = 0;
    let r: std::io::Result<()> = retry_with(&mut |_| {}, &|_| false, || {
        n += 1;
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    });
    assert!(r.is_err());
    assert_eq!(n, 1);
    // Off Windows nothing is transient.
    #[cfg(not(windows))]
    assert!(!oma_export::stage::is_transient(
        &std::io::Error::from_raw_os_error(32)
    ));
    #[cfg(windows)]
    assert!(oma_export::stage::is_transient(
        &std::io::Error::from_raw_os_error(32)
    ));
}

#[test]
fn debug_output_names_no_path_or_name() {
    let d = dest();
    let mut s = Staging::bundle(&d.parent).expect("staging");
    let attachment = oma_export::naming::attachment_name(
        1,
        Some("secret-name.png"),
        Some(oma_archive::SniffedKind::Png),
        120,
        false,
    );
    let p = BundlePath::attachment(&attachment);
    let mut f = s.create(&p).expect("create");
    f.write_all(b"x").expect("write");
    let shown = format!("{f:?} {p:?} {attachment:?} {s:?}");
    let entry = s.close(f).expect("close");
    let shown = format!(
        "{shown} {entry:?} {:?}",
        TargetName::new("secret-target").expect("n")
    );
    for leak in ["secret", "dest", ".png", "attachments"] {
        assert!(!shown.contains(leak), "{leak}: {shown}");
    }
    let _ = ExportZone::Utc;
    assert!(s.remove());
}
