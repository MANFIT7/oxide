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
    pub sha256_url: Option<String>,
    pub notes: String,
}

/// True if `a` is a newer semver than `b` (numeric dotted compare).
fn version_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split(['.', '-', '+'])
            .filter_map(|p| {
                p.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .ok()
            })
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
    let client = reqwest::Client::builder()
        .user_agent("oxide-updater")
        .build()
        .ok()?;
    let v: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;
    let tag = v["tag_name"].as_str()?;
    let version = tag.trim_start_matches('v').to_string();
    if !version_gt(&version, CURRENT) {
        return None;
    }
    let notes = v["body"]
        .as_str()
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect::<String>();
    let assets = v["assets"].as_array()?;
    let asset = pick_asset(assets)?;
    Some(UpdateInfo {
        version,
        sha256_url: checksum_url_for(assets, &asset),
        url: asset,
        notes,
    })
}

/// Pick the release asset matching the current OS/arch (macOS-arm64 first).
fn pick_asset(assets: &[serde_json::Value]) -> Option<String> {
    let os = std::env::consts::OS; // "macos"
    let arch = std::env::consts::ARCH; // "aarch64"
    let name_of = |a: &serde_json::Value| a["name"].as_str().unwrap_or("").to_ascii_lowercase();
    let dl = |a: &serde_json::Value| a["browser_download_url"].as_str().map(String::from);
    let wanted = match (os, arch) {
        ("macos", "aarch64") => "oxide-macos-arm64.gz",
        ("macos", "x86_64") => "oxide-macos-x64.gz",
        ("linux", "aarch64") => "oxide-linux-arm64.gz",
        ("linux", "x86_64") => "oxide-linux-x64.gz",
        ("windows", "x86_64") => "oxide-windows-x64.exe.gz",
        _ => return None,
    };
    assets.iter().find(|a| name_of(a) == wanted).and_then(dl)
}

fn checksum_url_for(assets: &[serde_json::Value], asset_url: &str) -> Option<String> {
    let asset_name = asset_url.rsplit('/').next()?.to_ascii_lowercase();
    let checksum_name = format!("{asset_name}.sha256");
    assets.iter().find_map(|a| {
        let name = a["name"].as_str()?.to_ascii_lowercase();
        if name == checksum_name {
            a["browser_download_url"].as_str().map(String::from)
        } else {
            None
        }
    })
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
        sha256_url: v["sha256_url"].as_str().map(str::to_string),
        notes: v["notes"].as_str().unwrap_or("").to_string(),
    })
}

/// Download the new binary (streamed, with progress 0.0–1.0) and replace the
/// running executable. A `.gz` asset is decompressed on the fly.
pub async fn apply<F: Fn(f32)>(info: &UpdateInfo, on_progress: F) -> anyhow::Result<()> {
    use futures::StreamExt;
    let client = reqwest::Client::builder()
        .user_agent("oxide-updater")
        .build()?;
    let resp = client.get(&info.url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("update download failed: {}", resp.status());
    }
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
    if let Some(expected) = expected_sha256(&client, info).await? {
        let actual = sha256_hex(&buf);
        if !actual.eq_ignore_ascii_case(&expected) {
            anyhow::bail!("update checksum mismatch: expected {expected}, got {actual}");
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

async fn expected_sha256(
    client: &reqwest::Client,
    info: &UpdateInfo,
) -> anyhow::Result<Option<String>> {
    let Some(url) = info.sha256_url.as_deref() else {
        return Ok(None);
    };
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("checksum download failed: {}", resp.status());
    }
    let text = resp.text().await?;
    let Some(first) = text.split_whitespace().next() else {
        anyhow::bail!("checksum file is empty");
    };
    if first.len() != 64 || !first.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("checksum file does not start with a SHA-256 hex digest");
    }
    Ok(Some(first.to_string()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Relaunch the (now-updated) executable and exit the current process.
pub fn restart() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("gui").spawn();
    }
    std::process::exit(0);
}
