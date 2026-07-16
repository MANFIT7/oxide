//! Headless/visible browser automation via CDP (chromiumoxide).
//!
//! Drives an installed Chromium-based browser for background web testing:
//! navigate, read text/DOM, click, type, screenshot, and run JS. One lazily
//! launched session per engine; a background task pumps the CDP event loop.

use anyhow::{anyhow, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::handler::viewport::Viewport;
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
    browser: Browser,
    page: Page,
    pump: JoinHandle<()>,
    profile_dir: PathBuf,
}

impl BrowserSession {
    /// Launch a browser session. `headless` hides the window for background use.
    pub async fn launch(headless: bool) -> Result<Self> {
        Self::launch_with_viewport(headless, None).await
    }

    pub async fn launch_with_viewport(
        headless: bool,
        viewport: Option<(u32, u32)>,
    ) -> Result<Self> {
        let mut builder = BrowserConfig::builder();
        if !headless {
            builder = builder.with_head();
        }
        let profile_dir = unique_browser_profile_dir();
        builder = builder.user_data_dir(profile_dir.clone());
        if let Some((width, height)) = viewport {
            builder = builder.viewport(Viewport {
                width,
                height,
                device_scale_factor: Some(1.0),
                emulating_mobile: false,
                is_landscape: width >= height,
                has_touch: false,
            });
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
            browser,
            page,
            pump,
            profile_dir,
        })
    }

    /// Close Chromium and remove its temporary profile after the turn that used
    /// browser tools finishes. Keeping the session alive within a turn preserves
    /// navigate/read/click sequences without leaving an idle browser on the host.
    pub async fn close(mut self) -> Result<()> {
        let close_result = self.browser.close().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), &mut self.pump).await;
        let _ = tokio::fs::remove_dir_all(&self.profile_dir).await;
        close_result.map(|_| ()).map_err(Into::into)
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
        let data = self.screenshot_png().await?;
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = dir.join(format!("shot-{ts}.png"));
        std::fs::write(&path, data)?;
        Ok(format!("screenshot saved → {}", path.display()))
    }

    pub async fn screenshot_png(&self) -> Result<Vec<u8>> {
        Ok(self
            .page
            .screenshot(ScreenshotParams::builder().build())
            .await?)
    }

    pub async fn eval(&self, script: &str) -> Result<String> {
        let v = self.page.evaluate(script).await?;
        match v.value() {
            Some(val) => Ok(serde_json::to_string(val).unwrap_or_else(|_| val.to_string())),
            None => Ok("undefined".to_string()),
        }
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

fn unique_browser_profile_dir() -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("oxide-browser-{}-{ts}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chromiumoxide::cdp::browser_protocol::emulation::{MediaFeature, SetEmulatedMediaParams};
    #[tokio::test]
    #[ignore] // needs an installed Chromium browser
    async fn smoke_navigate_read() {
        let s = BrowserSession::launch(true).await.expect("launch");
        let out = s.navigate("https://example.com").await.expect("navigate");
        assert!(out.to_lowercase().contains("example domain"), "got: {out}");
        let js = s.eval("1+2").await.expect("eval");
        assert_eq!(js, "3");
    }

    #[tokio::test]
    #[ignore] // needs python3 scripts/gui-visual-qa.py and an installed Chromium browser
    async fn gui_visual_fixture_screenshot() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .to_path_buf();
        let fixture = root.join("target/gui-visual-qa/fixture.html");
        assert!(
            fixture.exists(),
            "missing {}; run `python3 scripts/gui-visual-qa.py` first",
            fixture.display()
        );

        let s = BrowserSession::launch_with_viewport(true, Some((1280, 820)))
            .await
            .expect("launch browser");
        s.page
            .execute(
                SetEmulatedMediaParams::builder()
                    .feature(MediaFeature::new("prefers-reduced-motion", "no-preference"))
                    .build(),
            )
            .await
            .expect("emulate normal motion");
        let url = format!("file://{}", fixture.display());
        s.navigate(&url).await.expect("navigate visual fixture");

        let report = s
            .page
            .evaluate(
                r#"
(() => {
  const required = [
    '.streaming-message .agent-md.live',
    '.agent-md.live .live-tail .live-word.fresh',
    '.thinking-box[open] .thinking-body',
    '.thought-row.settling:not([open]) .thought-label-settled',
    '.act-group:not([open]) .act-group-head',
    '.activity-card.running .activity-status',
    '.activity-card.live-output[open] .activity-out',
    '.activity-card.has-out[open] .activity-out',
    '.review-actions .diff-kept',
    '.edits-row.pending .edits-rowcounts.shimmer',
    '.env-card-row.env-subagents-running .env-subagent-preview',
    '.todo-card.run-disclosure:not([open]) .run-preview',
    '.composer-live-changes .live-changes-head',
    '.skill-menu .skill-mention-mark',
    '.artifact-card .artifact-image',
    '.status-pill .status-shimmer'
  ];
  const missing = required.filter((selector) => !document.querySelector(selector));
  const thinking = document.querySelector('.thinking-box')?.getBoundingClientRect();
  const answer = document.querySelector('.row.agent:not(.agent-waiting)')?.getBoundingClientRect();
  const stream = document.querySelector('.agent-md.live');
  const streamWord = document.querySelector('.agent-md.live .live-word.fresh');
  const streamRow = document.querySelector('.streaming-message');
  const settledThoughtLabel = document.querySelector('.thought-row.settling .thought-label-settled');
  const settledThought = document.querySelector('.thought-row.settling');
  const workingGroup = document.querySelector('.act-group');
  const subagentsRow = document.querySelector('.env-card-row.env-subagents-running');
  const todoCard = document.querySelector('.todo-card.run-disclosure');
  const liveChangesCard = document.querySelector('.composer-live-changes');
  const skillMenu = document.querySelector('.skill-menu');
  const artifactCard = document.querySelector('.artifact-card');
  const artifactImage = document.querySelector('.artifact-card .artifact-image');
  const runningSpinner = document.querySelector('.activity-card.running .activity-spin');
  const settledSpinner = document.querySelector('.activity-card.done .activity-spin');
  const runningResult = document.querySelector('.activity-card.running .activity-ic.ok');
  const spinnerRect = runningSpinner?.getBoundingClientRect();
  const resultRect = runningResult?.getBoundingClientRect();
  return JSON.stringify({
    missing,
    thinkingAboveAnswer: Boolean(thinking && answer && thinking.bottom <= answer.top),
    streamAnimation: stream ? getComputedStyle(stream).animationName : '',
    streamWordAnimation: streamWord ? getComputedStyle(streamWord).animationName : '',
    streamRailAnimation: streamRow ? getComputedStyle(streamRow, '::before').animationName : '',
    thinkingShimmerAnimation: getComputedStyle(document.querySelector('.thinking-glow')).animationName,
    thinkingRevealAnimation: getComputedStyle(document.querySelector('.thinking-body')).animationName,
    settledThoughtLabelAnimation: settledThoughtLabel ? getComputedStyle(settledThoughtLabel).animationName : '',
    settledThoughtCollapsed: Boolean(settledThought && !settledThought.open),
    workingGroupCollapsed: Boolean(workingGroup && !workingGroup.open && workingGroup.getBoundingClientRect().height <= 44),
    toolLabelShimmerAnimation: getComputedStyle(document.querySelector('.activity-card.running .activity-verb')).animationName,
    liveEditShimmerAnimation: getComputedStyle(document.querySelector('.composer-live-changes .live-changes-title')).animationName,
    liveEditEntryAnimation: getComputedStyle(liveChangesCard).animationName,
    toolSpinnerAnimation: runningSpinner ? getComputedStyle(runningSpinner, '::after').animationName : '',
    spinnerHasFrames: runningSpinner ? getComputedStyle(runningSpinner, '::after').content.includes('⠋') && getComputedStyle(runningSpinner, '::after').content.includes('⠏') : false,
    spinnerChildCount: runningSpinner?.childElementCount ?? -1,
    runningSpinnerAnimationCount: runningSpinner?.getAnimations({subtree: true}).filter(a => a.animationName === 'oxide-unicode-frame').length ?? -1,
    settledSpinnerAnimationCount: settledSpinner?.getAnimations({subtree: true}).filter(a => a.animationName === 'oxide-unicode-frame').length ?? -1,
    compactOrchestrationCards: [subagentsRow, todoCard, liveChangesCard].every(card => card && card.getBoundingClientRect().height <= 48),
    skillMenuCompact: Boolean(skillMenu && skillMenu.getBoundingClientRect().height <= 140),
    artifactPreviewSized: Boolean(artifactCard && artifactImage && artifactCard.getBoundingClientRect().width >= 240 && artifactImage.getBoundingClientRect().height >= 120),
    statusSlotAligned: Boolean(spinnerRect && resultRect && Math.abs((spinnerRect.x + spinnerRect.width / 2) - (resultRect.x + resultRect.width / 2)) < 0.5 && Math.abs((spinnerRect.y + spinnerRect.height / 2) - (resultRect.y + resultRect.height / 2)) < 0.5),
    viewport: [window.innerWidth, window.innerHeight],
    text: document.body.innerText
  });
})()
"#,
            )
            .await
            .expect("eval fixture selectors")
            .into_value::<String>()
            .expect("selector report string");
        let report: serde_json::Value =
            serde_json::from_str(&report).expect("selector report json");
        assert_eq!(
            report["missing"].as_array().map(Vec::len),
            Some(0),
            "missing visual selectors: {report}"
        );
        assert_eq!(
            report["thinkingAboveAnswer"].as_bool(),
            Some(true),
            "reasoning block should stay above the live answer: {report}"
        );
        assert_eq!(
            report["streamAnimation"].as_str(),
            Some("oxide-stream-first-token"),
            "live answer should use the first-token entrance: {report}"
        );
        assert_eq!(
            report["streamWordAnimation"].as_str(),
            Some("oxide-stream-word"),
            "only the keyed live tail words should fade in: {report}"
        );
        assert_eq!(
            report["streamRailAnimation"].as_str(),
            Some("oxide-stream-rail"),
            "streaming rail should animate outside the live HTML: {report}"
        );
        assert_eq!(
            report["thinkingShimmerAnimation"].as_str(),
            Some("ox-shimmer"),
            "live reasoning label should shimmer: {report}"
        );
        assert_eq!(
            report["thinkingRevealAnimation"].as_str(),
            Some("oxide-reveal-down"),
            "expanded reasoning should reveal without a hard pop: {report}"
        );
        assert_eq!(
            report["settledThoughtLabelAnimation"].as_str(),
            Some("oxide-thought-settled-in"),
            "finished reasoning should cross-fade into its settled label: {report}"
        );
        assert_eq!(
            report["settledThoughtCollapsed"].as_bool(),
            Some(true),
            "finished Thought should not auto-expand its reasoning body: {report}"
        );
        assert_eq!(
            report["workingGroupCollapsed"].as_bool(),
            Some(true),
            "Working action groups should stay collapsed until explicitly opened: {report}"
        );
        assert_eq!(
            report["toolLabelShimmerAnimation"].as_str(),
            Some("ox-shimmer"),
            "running tool label should shimmer: {report}"
        );
        assert_eq!(
            report["liveEditShimmerAnimation"].as_str(),
            Some("ox-shimmer"),
            "live edit title should shimmer: {report}"
        );
        assert_eq!(
            report["liveEditEntryAnimation"].as_str(),
            Some("oxide-tool-enter"),
            "live edit summary should enter smoothly: {report}"
        );
        assert_eq!(
            report["toolSpinnerAnimation"].as_str(),
            Some("oxide-unicode-frame"),
            "running tool should animate Braille frames inside its stable status slot: {report}"
        );
        assert_eq!(
            report["spinnerHasFrames"].as_bool(),
            Some(true),
            "Braille spinner pseudo-element should contain the complete frame strip: {report}"
        );
        assert_eq!(
            report["spinnerChildCount"].as_i64(),
            Some(0),
            "Braille spinner should not allocate per-frame DOM children: {report}"
        );
        assert_eq!(
            report["runningSpinnerAnimationCount"].as_i64(),
            Some(1),
            "running Braille spinner should use exactly one animation timeline: {report}"
        );
        assert_eq!(
            report["settledSpinnerAnimationCount"].as_i64(),
            Some(0),
            "settled Braille spinner must not retain hidden animation timelines: {report}"
        );
        assert_eq!(
            report["compactOrchestrationCards"].as_bool(),
            Some(true),
            "Subagents, Tasks, and Changing files should stay one-line until expanded: {report}"
        );
        assert_eq!(
            report["skillMenuCompact"].as_bool(),
            Some(true),
            "$skill suggestions should stay compact above the composer: {report}"
        );
        assert_eq!(
            report["artifactPreviewSized"].as_bool(),
            Some(true),
            "generated image citations should render as useful preview cards: {report}"
        );
        assert_eq!(
            report["statusSlotAligned"].as_bool(),
            Some(true),
            "tool spinner and result icon should occupy the same fixed slot: {report}"
        );
        let text = report["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("Reasoning")
                && text.contains("Preparing")
                && text.contains("ask_user")
                && text.contains("audit-gui-motion")
                && text.contains("GUI evidence")
                && text.contains("Kept"),
            "fixture text did not render expected labels: {text}"
        );

        let png = s.screenshot_png().await.expect("screenshot");
        let out = root.join("target/gui-visual-qa/fixture-cdp.png");
        let _ = std::fs::write(&out, &png);
        let image = image::load_from_memory(&png)
            .expect("decode screenshot")
            .to_rgba8();
        assert!(
            image.width() >= 1000 && image.height() >= 700,
            "unexpected screenshot size {}x{}",
            image.width(),
            image.height()
        );

        let mut min_luma = u8::MAX;
        let mut max_luma = u8::MIN;
        let mut bright = 0usize;
        let mut sampled = 0usize;
        for (_, _, px) in image.enumerate_pixels().step_by(257) {
            let [r, g, b, _] = px.0;
            let luma = ((299u32 * r as u32 + 587u32 * g as u32 + 114u32 * b as u32) / 1000) as u8;
            min_luma = min_luma.min(luma);
            max_luma = max_luma.max(luma);
            if luma > 96 {
                bright += 1;
            }
            sampled += 1;
        }
        assert!(
            max_luma.saturating_sub(min_luma) >= 40 && bright >= 20 && sampled > 1000,
            "screenshot looks blank: contrast={}, bright={}, sampled={}, saved={}",
            max_luma.saturating_sub(min_luma),
            bright,
            sampled,
            out.display()
        );

        s.page
            .execute(
                SetEmulatedMediaParams::builder()
                    .feature(MediaFeature::new("prefers-reduced-motion", "reduce"))
                    .build(),
            )
            .await
            .expect("emulate reduced motion");
        let reduced = s
            .page
            .evaluate(
                r#"
JSON.stringify({
  streamAnimation: getComputedStyle(document.querySelector('.agent-md.live')).animationName,
  streamWordAnimation: getComputedStyle(document.querySelector('.agent-md.live .live-word.fresh')).animationName,
  streamRailAnimation: getComputedStyle(document.querySelector('.streaming-message'), '::before').animationName,
  thoughtSettleAnimation: getComputedStyle(document.querySelector('.thought-label-settled')).animationName,
  toolHaloAnimation: getComputedStyle(document.querySelector('.activity-card.running .activity-status'), '::after').animationName,
  toolSpinnerAnimation: getComputedStyle(document.querySelector('.activity-card.running .activity-spin'), '::after').animationName,
  thinkingShimmerAnimation: getComputedStyle(document.querySelector('.thinking-glow')).animationName,
  toolLabelShimmerAnimation: getComputedStyle(document.querySelector('.activity-card.running .activity-verb')).animationName,
  liveEditTitleShimmerAnimation: getComputedStyle(document.querySelector('.composer-live-changes .live-changes-title')).animationName,
  statusShimmerAnimation: getComputedStyle(document.querySelector('.status-pill .status-shimmer')).animationName,
  editShimmerAnimation: getComputedStyle(document.querySelector('.edits-row.pending .edits-rowcounts.shimmer')).animationName
})
"#,
            )
            .await
            .expect("eval host motion-preference styles")
            .into_value::<String>()
            .expect("host motion-preference report string");
        let reduced: serde_json::Value =
            serde_json::from_str(&reduced).expect("host motion-preference report json");
        assert_eq!(
            reduced["streamAnimation"].as_str(),
            Some("oxide-stream-first-token")
        );
        assert_eq!(
            reduced["streamWordAnimation"].as_str(),
            Some("oxide-stream-word")
        );
        assert_eq!(
            reduced["streamRailAnimation"].as_str(),
            Some("oxide-stream-rail")
        );
        assert_eq!(
            reduced["thoughtSettleAnimation"].as_str(),
            Some("oxide-thought-settled-in")
        );
        assert_eq!(reduced["toolHaloAnimation"].as_str(), Some("none"));
        assert_eq!(
            reduced["toolSpinnerAnimation"].as_str(),
            Some("oxide-unicode-frame")
        );
        assert_eq!(
            reduced["thinkingShimmerAnimation"].as_str(),
            Some("ox-shimmer")
        );
        assert_eq!(
            reduced["toolLabelShimmerAnimation"].as_str(),
            Some("ox-shimmer")
        );
        assert_eq!(
            reduced["liveEditTitleShimmerAnimation"].as_str(),
            Some("ox-shimmer")
        );
        assert_eq!(
            reduced["statusShimmerAnimation"].as_str(),
            Some("ox-shimmer")
        );
        assert_eq!(reduced["editShimmerAnimation"].as_str(), Some("shimmer"));
    }
}
