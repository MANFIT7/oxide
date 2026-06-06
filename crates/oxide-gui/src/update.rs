//! OTA self-update.
//!
//! Checks an update-manifest URL (`{ "version": "x.y.z", "url": "<binary>",
//! "notes": "..." }`); if the remote version is newer than the running build,
//! downloads the binary and swaps the running executable in place (via
//! `self-replace`). The caller restarts the app.

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, PartialEq)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    pub notes: String,
}

/// True if `a` is a newer semver than `b` (numeric dotted compare).
fn version_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split(['.', '-', '+'])
            .filter_map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok())
            .collect()
    };
    let (a, b) = (parse(a), parse(b));
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// Check for updates — prefers a GitHub repo (`owner/name`), falls back to a
/// custom manifest URL.
pub async fn check(github_repo: &str, manifest_url: &str) -> Option<UpdateInfo> {
    if !github_repo.trim().is_empty() {
        if let Some(info) = check_github(github_repo.trim()).await {
            return Some(info);
        }
    }
    if !manifest_url.trim().is_empty() {
        return check_manifest(manifest_url).await;
    }
    None
}

/// Read the latest GitHub release and pick the binary asset for this platform.
async fn check_github(repo: &str) -> Option<UpdateInfo> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let client = reqwest::Client::builder().user_agent("oxide-updater").build().ok()?;
    let v: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;
    let tag = v["tag_name"].as_str()?;
    let version = tag.trim_start_matches('v').to_string();
    if !version_gt(&version, CURRENT) {
        return None;
    }
    let notes = v["body"].as_str().unwrap_or("").lines().next().unwrap_or("").chars().take(120).collect::<String>();
    let assets = v["assets"].as_array()?;
    let asset = pick_asset(assets)?;
    Some(UpdateInfo { version, url: asset, notes })
}

/// Pick the release asset matching the current OS/arch (macOS-arm64 first).
fn pick_asset(assets: &[serde_json::Value]) -> Option<String> {
    let os = std::env::consts::OS; // "macos"
    let arch = std::env::consts::ARCH; // "aarch64"
    let name_of = |a: &serde_json::Value| a["name"].as_str().unwrap_or("").to_ascii_lowercase();
    let dl = |a: &serde_json::Value| a["browser_download_url"].as_str().map(String::from);
    let os_match = |n: &str| n.contains(os) || (os == "macos" && n.contains("darwin"));
    let arch_match = |n: &str| n.contains(arch) || (arch == "aarch64" && n.contains("arm64"));
    // best: os + arch + gzip (smallest download)
    if let Some(a) = assets.iter().find(|a| { let n = name_of(a); os_match(&n) && arch_match(&n) && n.ends_with(".gz") }) {
        return dl(a);
    }
    // os + arch (raw binary)
    if let Some(a) = assets.iter().find(|a| { let n = name_of(a); os_match(&n) && arch_match(&n) && !n.ends_with(".dmg") }) {
        return dl(a);
    }
    // os only
    if let Some(a) = assets.iter().find(|a| os_match(&name_of(a))) {
        return dl(a);
    }
    assets.first().and_then(dl)
}

async fn check_manifest(manifest_url: &str) -> Option<UpdateInfo> {
    let v: serde_json::Value = reqwest::get(manifest_url).await.ok()?.json().await.ok()?;
    let version = v["version"].as_str()?.to_string();
    if !version_gt(&version, CURRENT) {
        return None;
    }
    Some(UpdateInfo {
        version,
        url: v["url"].as_str()?.to_string(),
        notes: v["notes"].as_str().unwrap_or("").to_string(),
    })
}

/// Download the new binary (streamed, with progress 0.0–1.0) and replace the
/// running executable. A `.gz` asset is decompressed on the fly.
pub async fn apply<F: Fn(f32)>(info: &UpdateInfo, on_progress: F) -> anyhow::Result<()> {
    use futures::StreamExt;
    let client = reqwest::Client::builder().user_agent("oxide-updater").build()?;
    let resp = client.get(&info.url).send().await?;
    let total = resp.content_length().unwrap_or(0);
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    let mut got: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        got += chunk.len() as u64;
        buf.extend_from_slice(&chunk);
        if total > 0 {
            on_progress((got as f32 / total as f32).min(0.98));
        }
    }
    // Decompress gzip assets in memory.
    let data = if info.url.ends_with(".gz") {
        use std::io::Read;
        let mut d = flate2::read::GzDecoder::new(&buf[..]);
        let mut out = Vec::new();
        d.read_to_end(&mut out)?;
        out
    } else {
        buf
    };
    let tmp = std::env::temp_dir().join("oxide-update-bin");
    std::fs::write(&tmp, &data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    self_replace::self_replace(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    on_progress(1.0);
    Ok(())
}

/// Relaunch the (now-updated) executable and exit the current process.
pub fn restart() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("gui").spawn();
    }
    std::process::exit(0);
}
