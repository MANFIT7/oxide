//! Headless/visible browser automation via CDP (chromiumoxide).
//!
//! Drives an installed Chromium-based browser for background web testing:
//! navigate, read text/DOM, click, type, screenshot, and run JS. One lazily
//! launched session per engine; a background task pumps the CDP event loop.

use anyhow::{anyhow, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use futures::StreamExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::task::JoinHandle;

/// Locate an installed Chromium-based browser binary (macOS first).
pub fn detect_browser() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("OXIDE_BROWSER_BIN") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    // macOS .app bundles — the executable in Contents/MacOS may be renamed
    // (e.g. a Chromium fork), so scan for the actual binary rather than guess.
    let apps = [
        "/Applications/Google Chrome.app",
        "/Applications/Chromium.app",
        "/Applications/Brave Browser.app",
        "/Applications/Microsoft Edge.app",
        "/Applications/Dia.app",
        "/Applications/Arc.app",
        "/Applications/Vivaldi.app",
        "/Applications/Google Chrome Canary.app",
    ];
    for app in apps {
        let macos = PathBuf::from(app).join("Contents/MacOS");
        if let Ok(rd) = std::fs::read_dir(&macos) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    // Linux direct binaries.
    [
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/brave-browser",
        "/usr/bin/microsoft-edge",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

pub struct BrowserSession {
    _browser: Browser,
    page: Page,
    _pump: JoinHandle<()>,
}

impl BrowserSession {
    /// Launch a browser session. `headless` hides the window for background use.
    pub async fn launch(headless: bool) -> Result<Self> {
        let mut builder = BrowserConfig::builder();
        if !headless {
            builder = builder.with_head();
        }
        if let Some(bin) = detect_browser() {
            builder = builder.chrome_executable(bin);
        }
        let config = builder
            .build()
            .map_err(|e| anyhow!("browser config: {e}"))?;
        let (browser, mut handler) = Browser::launch(config).await?;
        let pump = tokio::spawn(async move { while handler.next().await.is_some() {} });
        let page = browser.new_page("about:blank").await?;
        Ok(Self {
            _browser: browser,
            page,
            _pump: pump,
        })
    }

    pub async fn navigate(&self, url: &str) -> Result<String> {
        self.page.goto(url).await?;
        // wait_for_navigation has no built-in timeout — cap it so a stalled
        // page load can't freeze the engine indefinitely.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            self.page.wait_for_navigation(),
        )
        .await;
        let title = self
            .page
            .get_title()
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let text = self.read_text().await.unwrap_or_default();
        Ok(format!(
            "navigated → {url}\ntitle: {title}\n\n{}",
            truncate(&text, 1800)
        ))
    }

    pub async fn read_text(&self) -> Result<String> {
        let v = self
            .page
            .evaluate("document.body ? document.body.innerText : ''")
            .await?;
        Ok(v.into_value::<String>().unwrap_or_default())
    }

    pub async fn click(&self, selector: &str) -> Result<String> {
        let el = self.page.find_element(selector).await?;
        el.click().await?;
        let _ = self.page.wait_for_navigation().await;
        Ok(format!("clicked {selector}"))
    }

    pub async fn type_text(&self, selector: &str, text: &str) -> Result<String> {
        let el = self.page.find_element(selector).await?;
        el.click().await?;
        el.type_str(text).await?;
        Ok(format!("typed {} char(s) into {selector}", text.len()))
    }

    pub async fn screenshot(&self, dir: &std::path::Path) -> Result<String> {
        std::fs::create_dir_all(dir).ok();
        let data = self
            .page
            .screenshot(ScreenshotParams::builder().build())
            .await?;
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = dir.join(format!("shot-{ts}.png"));
        std::fs::write(&path, data)?;
        Ok(format!("screenshot saved → {}", path.display()))
    }

    pub async fn eval(&self, script: &str) -> Result<String> {
        let v = self.page.evaluate(script).await?;
        match v.value() {
            Some(val) => Ok(serde_json::to_string(val).unwrap_or_else(|_| val.to_string())),
            None => Ok("undefined".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore] // needs an installed Chromium browser
    async fn smoke_navigate_read() {
        let s = BrowserSession::launch(true).await.expect("launch");
        let out = s.navigate("https://example.com").await.expect("navigate");
        assert!(out.to_lowercase().contains("example domain"), "got: {out}");
        let js = s.eval("1+2").await.expect("eval");
        assert_eq!(js, "3");
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let t: String = s.chars().take(max).collect();
        format!("{t}\n…(truncated)")
    } else {
        s.to_string()
    }
}
