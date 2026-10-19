//! The main window, built in Rust (M5 spec §6.7, D106).
//!
//! `tauri.conf.json` declares `main` with `"create": false` and
//! `"incognito": true`, and `setup` builds it here with four hooks, so
//! the exits a page could otherwise use are closed in Rust:
//!
//! - **navigation** only to the app's own origin (plus the dev server's in
//!   a dev build): a page bug cannot move the window to another document;
//! - **new windows** are denied (`window.open` is not used, and the CSP's
//!   `frame-src 'none'` stays);
//! - **downloads** are refused: without a handler both engines may save
//!   to the user's Downloads folder, a plaintext copy outside the archive
//!   (saving a copy of an attachment is M6);
//! - **permission requests** (camera, microphone, location, …) are denied.
//!
//! The policies are plain functions, tested without a window; only
//! [`build_main`] touches Tauri. The window title stays "OpenMessage
//! Archive" (never a chat name, D112).

/// The main window's label.
pub const MAIN: &str = "main";

/// The page origin the app serves its frontend from: `tauri://localhost`
/// on Linux, `http://tauri.localhost` on Windows (`use_https_scheme` is
/// false).
pub const fn app_origin() -> &'static str {
    if cfg!(windows) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    }
}

/// `scheme://host[:port]`, lowercased; `None` without a host.
pub fn origin_of(scheme: &str, host: Option<&str>, port: Option<u16>) -> Option<String> {
    let host = host?;
    if host.is_empty() {
        return None;
    }
    let mut o = format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    );
    if let Some(p) = port {
        o.push(':');
        o.push_str(&p.to_string());
    }
    Some(o)
}

/// The accepted page origins: the app's, plus the dev server's when given
/// (dev builds only pass one).
pub fn allowed_origins(dev_origin: Option<String>) -> Vec<String> {
    let mut v = vec![app_origin().to_owned()];
    v.extend(dev_origin);
    v
}

/// `on_navigation`: only to an accepted origin.
pub fn navigation_allowed(
    origins: &[String],
    scheme: &str,
    host: Option<&str>,
    port: Option<u16>,
) -> bool {
    origin_of(scheme, host, port).is_some_and(|o| origins.contains(&o))
}

/// `on_download`: never.
pub const fn download_allowed() -> bool {
    false
}

/// `on_new_window`: never.
pub const fn new_window_allowed() -> bool {
    false
}

/// `on_permission_request`: never.
pub const fn permission_granted() -> bool {
    false
}

/// Build the main window from its configuration, with the hooks above.
/// Returns the accepted origins, which the media protocol checks too.
pub fn build_main(app: &tauri::App) -> Result<(tauri::WebviewWindow, Vec<String>), String> {
    use tauri::webview::{NewWindowResponse, PermissionResponse};
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == MAIN)
        .cloned()
        .ok_or_else(|| "the main window is not configured".to_owned())?;
    let dev_origin = if cfg!(dev) {
        app.config()
            .build
            .dev_url
            .as_ref()
            .and_then(|u| origin_of(u.scheme(), u.host_str(), u.port()))
    } else {
        None
    };
    let origins = allowed_origins(dev_origin);
    let nav = origins.clone();
    let window = tauri::WebviewWindowBuilder::from_config(app.handle(), &config)
        .map_err(|e| e.to_string())?
        .on_navigation(move |url| {
            navigation_allowed(&nav, url.scheme(), url.host_str(), url.port())
        })
        .on_new_window(|_, _| {
            if new_window_allowed() {
                NewWindowResponse::Allow
            } else {
                NewWindowResponse::Deny
            }
        })
        .on_download(|_, _| download_allowed())
        .on_permission_request(|_, _| {
            if permission_granted() {
                PermissionResponse::Allow
            } else {
                PermissionResponse::Deny
            }
        })
        .build()
        .map_err(|e| e.to_string())?;
    Ok((window, origins))
}

#[cfg(test)]
mod tests;
