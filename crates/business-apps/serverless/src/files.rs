//! Deployable-build helpers for spaces: object-key builders, the `staging →
//! dist` deploy copy, and the static-file content-type resolver.
//!
//! A space's frontend lives in MinIO under `srv/{board_id}/{staging|dist}/…`;
//! uploaded files are staged, tested via the staff preview route, then
//! copied to `dist/` on deploy (the storage client has no server-side copy,
//! so deploy streams list → get → put). The public `/s/{board}` route serves
//! `dist/` with `index.html` as the SPA fallback.

use bytes::Bytes;
use storage::Storage;

/// The object key of one staged build file.
pub fn staging_key(board_id: &str, path: &str) -> String {
    format!("srv/{board_id}/staging/{path}")
}

/// The object key of one deployed build file.
pub fn dist_key(board_id: &str, path: &str) -> String {
    format!("srv/{board_id}/dist/{path}")
}

/// Rebuild a safe relative path from a browser directory-upload filename.
///
/// `webkitdirectory` uploads send the full relative path (e.g.
/// `C:\fakepath\assets\app.js` — the real relative path trails the last
/// separator). Every segment is sanitized; `..`, separators, NUL and empty
/// segments are rejected so paths can never escape `srv/{board}/…`.
pub fn sanitize_build_path(file_name: &str) -> Option<String> {
    let tail = file_name
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let mut clean: Vec<String> = Vec::with_capacity(tail.len());
    let mut skip_fakepath = false;
    for segment in tail {
        // Browsers may prefix the relative path with `C:\fakepath\` — skip
        // the fake drive root and its `fakepath` marker instead of
        // rejecting the file.
        if segment.len() == 2 && segment.ends_with(':') {
            skip_fakepath = true;
            continue;
        }
        if skip_fakepath && segment == "fakepath" {
            skip_fakepath = false;
            continue;
        }
        skip_fakepath = false;
        if segment == "." || segment == ".." || segment.contains('\0') {
            return None;
        }
        if segment.chars().any(|c| c.is_control()) {
            return None;
        }
        clean.push(segment.to_string());
    }
    let path = clean.join("/");
    if path.is_empty() || path.len() > 512 {
        None
    } else {
        Some(path)
    }
}

/// Sanitize a browser directory-upload filename and strip a leading `dist/`
/// segment, so uploading the local `dist/` folder itself lands files at the
/// build root (`staging/index.html`, not `staging/dist/index.html`).
pub fn normalize_upload_path(file_name: &str) -> Option<String> {
    let path = sanitize_build_path(file_name)?;
    let mut segments: Vec<&str> = path.split('/').collect();
    while segments.first() == Some(&"dist") {
        segments.remove(0);
    }
    let path = segments.join("/");
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// Content type for a static build file by extension.
pub fn content_type(path: &str) -> &'static str {
    let ext = path.split('.').next_back().unwrap_or_default();
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// True for assets that must not be cached aggressively between deploys.
pub fn is_immutable_asset(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/assets/") && (lower.ends_with(".js") || lower.ends_with(".css"))
}

/// Resolve a requested path inside a build root: empty → `index.html`,
/// else the path itself.
pub fn resolve_index<'a>(root: &str, path: Option<&'a str>) -> String {
    match path.filter(|p| !p.is_empty()) {
        Some(p) => p.to_string(),
        None => root.to_string(),
    }
}

/// Copy every object under `staging/` to `dist/` (list → get → put) and
/// return the number of files deployed. Missing staging roots yield 0.
pub async fn deploy_staging_to_dist(
    storage: &Storage,
    board_id: &str,
) -> Result<u64, storage::Error> {
    let prefix = format!("srv/{board_id}/staging/");
    let keys = storage.list(&prefix).await?;
    let mut deployed = 0u64;
    for key in keys {
        let Some(rest) = key.strip_prefix(&prefix) else {
            continue;
        };
        let Some(bytes) = storage.get_bytes(&key).await? else {
            continue;
        };
        storage
            .put(&dist_key(board_id, rest), bytes, content_type(rest))
            .await?;
        deployed += 1;
    }
    Ok(deployed)
}

/// A staged/deployed file pair read for streaming.
pub struct BuildFile {
    pub bytes: Bytes,
    pub content_type: &'static str,
}

/// Fetch one build file from `staging/` or `dist/` with index-fallback.
pub async fn get_build_file(
    storage: &Storage,
    board_id: &str,
    root: &str,
    path: Option<&str>,
) -> Result<Option<BuildFile>, storage::Error> {
    let key_of = |p: &str| {
        if root == "dist" {
            dist_key(board_id, p)
        } else {
            staging_key(board_id, p)
        }
    };
    match path.filter(|p| !p.is_empty()) {
        Some(p) => {
            if let Some(bytes) = storage.get_bytes(&key_of(p)).await? {
                return Ok(Some(BuildFile {
                    bytes,
                    content_type: content_type(p),
                }));
            }
        }
        None => {}
    }
    let Some(index) = storage.get_bytes(&key_of("index.html")).await? else {
        return Ok(None);
    };
    Ok(Some(BuildFile {
        bytes: index,
        content_type: content_type("index.html"),
    }))
}