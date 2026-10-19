//! The window policies, the merged per-platform configuration (M5 spec
//! §6.5, §6.6, §13.7) and the `protocol-asset` feature check.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use super::*;

fn conf_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(name: &str) -> Value {
    let p = conf_dir().join(name);
    serde_json::from_slice(&std::fs::read(&p).expect("config file")).expect("json")
}

/// JSON Merge Patch (RFC 7396), the way tauri-utils merges a platform
/// file over `tauri.conf.json` (`config/parse.rs`, `json_patch::merge`).
fn merge(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(p) => {
            if !target.is_object() {
                *target = Value::Object(serde_json::Map::new());
            }
            if let Value::Object(t) = target {
                for (k, v) in p {
                    if v.is_null() {
                        t.remove(k);
                    } else {
                        merge(t.entry(k.clone()).or_insert(Value::Null), v);
                    }
                }
            }
        }
        other => *target = other.clone(),
    }
}

fn merged(platform: &str) -> Value {
    let mut base = read_json("tauri.conf.json");
    merge(
        &mut base,
        &read_json(&format!("tauri.{platform}.conf.json")),
    );
    base
}

fn directives(v: &Value) -> BTreeMap<String, String> {
    v.as_object()
        .expect("a CSP map")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().expect("a source list").to_owned()))
        .collect()
}

fn expected(dev: bool, media: &str) -> BTreeMap<String, String> {
    let mut m: BTreeMap<String, String> = [
        ("default-src", "'self'"),
        ("script-src", "'self'"),
        ("font-src", "'self'"),
        ("object-src", "'none'"),
        ("base-uri", "'none'"),
        ("form-action", "'none'"),
        ("frame-src", "'none'"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    m.insert(
        "style-src".to_owned(),
        if dev {
            "'self' 'unsafe-inline'".to_owned()
        } else {
            "'self'".to_owned()
        },
    );
    m.insert(
        "connect-src".to_owned(),
        if dev {
            "ipc: http://ipc.localhost ws://localhost:1420 http://localhost:1420".to_owned()
        } else {
            "ipc: http://ipc.localhost".to_owned()
        },
    );
    m.insert("img-src".to_owned(), format!("'self' data: {media}"));
    m.insert("media-src".to_owned(), media.to_owned());
    m
}

/// §13.7: for each platform, the merged CSP and dev CSP are exactly the
/// expected maps (one scheme source added to `img-src` and `media-src`,
/// nothing else changed), the other platform's form is absent, and the
/// window and asset settings are as designed.
#[test]
fn the_merged_platform_configs_are_exactly_as_designed() {
    for (platform, media, foreign) in [
        ("linux", "oma-media:", "http://oma-media.localhost"),
        ("windows", "http://oma-media.localhost", "oma-media:"),
    ] {
        let c = merged(platform);
        let sec = &c["app"]["security"];
        assert_eq!(
            directives(&sec["csp"]),
            expected(false, media),
            "{platform} csp"
        );
        assert_eq!(
            directives(&sec["devCsp"]),
            expected(true, media),
            "{platform} devCsp"
        );
        let text = serde_json::to_string(sec).expect("json");
        assert!(!text.contains(foreign), "{platform} names {foreign}");
        assert!(
            sec.get("assetProtocol").is_none(),
            "{platform}: no asset protocol"
        );
        assert_eq!(sec["freezePrototype"], true);
        let windows = c["app"]["windows"].as_array().expect("windows");
        assert_eq!(windows.len(), 1);
        let main = &windows[0];
        assert_eq!(main["label"], MAIN);
        assert_eq!(main["create"], false, "{platform}: built in Rust");
        assert_eq!(main["incognito"], true, "{platform}: incognito");
        assert!(
            main.get("zoomHotkeysEnabled").is_none() || main["zoomHotkeysEnabled"] == false,
            "{platform}: zoom hotkeys stay off (D116)"
        );
        assert_eq!(main["title"], "OpenMessage Archive", "never a chat name");
        assert!(c["app"].get("withGlobalTauri") == Some(&Value::Bool(false)));
    }
    // The base file names neither scheme form: each platform adds its own.
    let base = serde_json::to_string(&read_json("tauri.conf.json")).expect("json");
    assert!(!base.contains("oma-media"));
}

/// The base the protocol hands out matches the platform's CSP form.
#[test]
fn the_media_base_matches_this_platforms_csp() {
    let base = crate::viewer::media::media_base();
    if cfg!(windows) {
        assert_eq!(base, "http://oma-media.localhost/");
    } else {
        assert_eq!(base, "oma-media://localhost/");
    }
    let platform = if cfg!(windows) { "windows" } else { "linux" };
    let c = merged(platform);
    let media_src = c["app"]["security"]["csp"]["media-src"]
        .as_str()
        .expect("media-src")
        .to_owned();
    let scheme_part = base.trim_end_matches('/').replace("://localhost", ":");
    assert!(
        media_src == scheme_part || media_src == base.trim_end_matches('/'),
        "{media_src} vs {base}"
    );
}

/// §13.7: `tauri` is built without `protocol-asset` (the asset protocol
/// would let the webview name files by path; I4).
#[test]
fn tauri_is_built_without_the_asset_protocol() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let manifest = conf_dir().join("Cargo.toml");
    let out = std::process::Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .expect("cargo metadata");
    assert!(out.status.success(), "cargo metadata failed");
    let meta: Value = serde_json::from_slice(&out.stdout).expect("json");
    let packages = meta["packages"].as_array().expect("packages");
    let tauri_id = packages
        .iter()
        .find(|p| p["name"] == "tauri")
        .and_then(|p| p["id"].as_str())
        .expect("tauri package")
        .to_owned();
    let node = meta["resolve"]["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == tauri_id.as_str())
        .expect("tauri node");
    let features: Vec<&str> = node["features"]
        .as_array()
        .expect("features")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(!features.is_empty(), "the resolve lists tauri's features");
    assert!(
        !features.contains(&"protocol-asset"),
        "tauri must not enable protocol-asset"
    );
    let shell = std::fs::read_to_string(&manifest).expect("Cargo.toml");
    assert!(!shell.contains("protocol-asset"));
}

#[test]
fn navigation_is_allowed_to_the_app_origin_only() {
    let app = app_origin();
    let (scheme, host) = app.split_once("://").expect("origin");
    let only = allowed_origins(None);
    assert!(navigation_allowed(&only, scheme, Some(host), None));
    assert!(navigation_allowed(
        &only,
        &scheme.to_ascii_uppercase(),
        Some(&host.to_ascii_uppercase()),
        None
    ));
    for (s, h, p) in [
        ("https", Some("example.com"), None),
        ("http", Some("localhost"), Some(1420)),
        ("file", None, None),
        ("about", None, None),
        ("data", None, None),
        (scheme, Some("localhost.evil"), None),
        (scheme, Some(host), Some(8080)),
        ("oma-media", Some("localhost"), None),
        ("http", Some("ipc.localhost"), None),
    ] {
        assert!(!navigation_allowed(&only, s, h, p), "{s} {h:?} {p:?}");
    }
    // A dev build adds the dev server's origin, and only it.
    let dev = allowed_origins(origin_of("http", Some("localhost"), Some(1420)));
    assert!(navigation_allowed(
        &dev,
        "http",
        Some("localhost"),
        Some(1420)
    ));
    assert!(!navigation_allowed(
        &dev,
        "http",
        Some("localhost"),
        Some(1421)
    ));
    assert!(!navigation_allowed(&dev, "http", Some("localhost"), None));
}

#[test]
fn downloads_new_windows_and_permissions_are_denied() {
    assert!(!download_allowed());
    assert!(!new_window_allowed());
    assert!(!permission_granted());
    // setup builds the window through these hooks.
    let lib = std::fs::read_to_string(conf_dir().join("src").join("window.rs")).expect("src");
    for hook in [
        ".on_navigation(",
        ".on_new_window(",
        ".on_download(",
        ".on_permission_request(",
        "NewWindowResponse::Deny",
        "PermissionResponse::Deny",
    ] {
        assert!(lib.contains(hook), "{hook}");
    }
    let main = std::fs::read_to_string(conf_dir().join("src").join("lib.rs")).expect("src");
    assert!(main.contains("window::build_main(app)"));
    assert!(main.contains("register_asynchronous_uri_scheme_protocol(viewer::media::SCHEME"));
}
