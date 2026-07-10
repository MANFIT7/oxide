use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const DEFAULT_NAME: &str = "Daily workspace review";
pub const DEFAULT_SCHEDULE: &str = "FREQ=DAILY;INTERVAL=1";
pub const DEFAULT_PROMPT: &str =
    "Review this workspace for recent changes, risky TODOs, failing checks, and useful next actions.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationSpec {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub schedule: String,
    pub prompt: String,
    pub created_ms: u64,
    /// Optional pre-run shell script (hermes: "mechanical work in the script,
    /// reasoning in the agent") — its stdout is injected into the run prompt.
    #[serde(default)]
    pub script: Option<String>,
    /// Shared secret for the local webhook trigger (`POST /hook/{id}` with
    /// header `x-oxide-token`). Set on creation; None on legacy specs.
    #[serde(default)]
    pub webhook_token: Option<String>,
    /// Optional session id whose recent transcript is injected on every run.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Deterministic webhook token (not cryptographic — the listener binds to
/// 127.0.0.1 only; this guards against accidental cross-automation firing).
pub fn webhook_token_for(id: &str, created_ms: u64) -> String {
    use std::hash::{Hash, Hasher};
    let mut a = std::collections::hash_map::DefaultHasher::new();
    (id, created_ms, "oxide-webhook-a").hash(&mut a);
    let mut b = std::collections::hash_map::DefaultHasher::new();
    (created_ms, id, "oxide-webhook-b").hash(&mut b);
    format!("{:016x}{:016x}", a.finish(), b.finish())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationRunSpec {
    pub id: String,
    pub automation_id: String,
    pub automation_name: String,
    pub trigger: String,
    pub status: String,
    pub prompt: String,
    pub started_ms: u64,
}

pub fn new_spec(name: &str, schedule: &str, prompt: &str, created_ms: u64) -> AutomationSpec {
    AutomationSpec {
        id: id_from_name(name, created_ms),
        name: name.trim().to_string(),
        kind: "cron".to_string(),
        status: "ACTIVE".to_string(),
        schedule: schedule.trim().to_string(),
        prompt: prompt.trim().to_string(),
        created_ms,
        script: None,
        webhook_token: Some(webhook_token_for(
            &id_from_name(name, created_ms),
            created_ms,
        )),
        session_id: None,
    }
}

pub fn read_specs(workspace: &Path) -> anyhow::Result<Vec<AutomationSpec>> {
    let dir = workspace.join(".oxide/automations");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut specs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        specs.push(toml::from_str::<AutomationSpec>(&text)?);
    }
    specs.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(specs)
}

pub fn write_spec(workspace: &Path, spec: &AutomationSpec) -> anyhow::Result<()> {
    let dir = workspace.join(".oxide/automations");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", spec.id));
    let text = toml::to_string_pretty(spec)?;
    std::fs::write(path, text)?;
    Ok(())
}

pub fn delete_spec(workspace: &Path, id: &str) -> anyhow::Result<()> {
    let path = workspace
        .join(".oxide/automations")
        .join(format!("{id}.toml"));
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn read_runs(workspace: &Path) -> anyhow::Result<Vec<AutomationRunSpec>> {
    let dir = workspace.join(".oxide/automation-runs");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        runs.push(toml::from_str::<AutomationRunSpec>(&text)?);
    }
    runs.sort_by(|a, b| {
        b.started_ms
            .cmp(&a.started_ms)
            .then_with(|| a.automation_name.cmp(&b.automation_name))
    });
    Ok(runs)
}

pub fn write_run(workspace: &Path, run: &AutomationRunSpec) -> anyhow::Result<()> {
    let dir = workspace.join(".oxide/automation-runs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", run.id));
    let text = toml::to_string_pretty(run)?;
    std::fs::write(path, text)?;
    Ok(())
}

pub fn with_toggled_status(spec: &AutomationSpec) -> AutomationSpec {
    let mut next = spec.clone();
    next.status = if spec.status == "ACTIVE" {
        "PAUSED".to_string()
    } else {
        "ACTIVE".to_string()
    };
    next
}

pub fn build_run_prompt(spec: &AutomationSpec) -> String {
    format!(
        "Run automation now\n\nName: {}\nKind: {}\nSchedule: {}\nStatus: {}\n\nAutomation prompt:\n{}\n\nDelivery rule: if this run produced NOTHING that needs the user's attention (no changes, nothing actionable), START your final reply with [SILENT].",
        spec.name, spec.kind, spec.schedule, spec.status, spec.prompt
    )
}

/// Full run prompt: pre-run script output (if configured) + webhook payload
/// (if this run was webhook-triggered) appended to [`build_run_prompt`].
pub async fn build_run_prompt_full(
    workspace: &Path,
    spec: &AutomationSpec,
    payload: Option<&str>,
) -> String {
    let mut prompt = build_run_prompt(spec);
    if let Some(script) = spec
        .script
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .current_dir(workspace)
                .output(),
        )
        .await;
        let text = match out {
            Ok(Ok(o)) => {
                let mut t = String::from_utf8_lossy(&o.stdout).to_string();
                if !o.status.success() {
                    t.push_str(&format!(
                        "\n[script exited {}] {}",
                        o.status.code().unwrap_or(-1),
                        String::from_utf8_lossy(&o.stderr)
                    ));
                }
                t
            }
            Ok(Err(e)) => format!("[script failed to run: {e}]"),
            Err(_) => "[script timed out after 60s]".to_string(),
        };
        let text: String = text.chars().take(8_000).collect();
        prompt.push_str(&format!("\n\nPre-run script output:\n{text}"));
    }
    if let Some(body) = payload.filter(|b| !b.trim().is_empty()) {
        let body: String = body.chars().take(4_000).collect();
        prompt.push_str(&format!("\n\nWebhook payload:\n{body}"));
    }
    if let Some(session_id) = spec.session_id.as_deref() {
        let rows = crate::db::load(session_id);
        let context = rows
            .into_iter()
            .rev()
            .filter(|(role, _)| matches!(role.as_str(), "user" | "assistant" | "summary"))
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|(role, content)| format!("{role}: {content}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        if !context.trim().is_empty() {
            let context: String = context.chars().take(12_000).collect();
            prompt.push_str(&format!(
                "\n\nBound thread context ({session_id}):\n{context}\n\nContinue this thread's intent; do not repeat already completed work."
            ));
        }
    }
    prompt
}

pub fn run_from_spec(
    spec: &AutomationSpec,
    trigger: &str,
    status: &str,
    started_ms: u64,
) -> AutomationRunSpec {
    AutomationRunSpec {
        id: format!("{}-{}-{started_ms}", slug_fragment(&spec.name), trigger),
        automation_id: spec.id.clone(),
        automation_name: spec.name.clone(),
        trigger: trigger.to_string(),
        status: status.to_string(),
        prompt: spec.prompt.clone(),
        started_ms,
    }
}

pub fn is_due(spec: &AutomationSpec, runs: &[AutomationRunSpec], now_ms: u64) -> bool {
    next_due(spec, runs).is_some_and(|at| at <= now_ms)
}

const DAY_MS: i64 = 86_400_000;
const MIN_MS: i64 = 60_000;

struct Parsed {
    once: Option<u64>,
    freq: Option<String>,
    interval: u64,
    /// Minute-of-day for time-anchored schedules (`AT=HH:MM`), else None.
    at_min: Option<u32>,
    /// Timezone offset east of UTC, in minutes (from `TZ=±HH:MM`); 0 = UTC.
    tz_off: i32,
    /// Weekday set for WEEKLY (0=Sun..6=Sat), from `BYDAY=MO,WE,FR`.
    byday: Vec<u8>,
    /// Plain-interval period (no time-of-day), reused from [`interval_ms`].
    period_ms: Option<u64>,
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    (h < 24 && m < 60).then_some(h * 60 + m)
}

fn parse_tz(s: &str) -> Option<i32> {
    let s = s.trim();
    let (sign, rest) = if let Some(r) = s.strip_prefix('-') {
        (-1, r)
    } else {
        (1, s.strip_prefix('+').unwrap_or(s))
    };
    let (h, m) = rest.split_once(':').unwrap_or((rest, "0"));
    let h: i32 = h.trim().parse().ok()?;
    let m: i32 = m.trim().parse().ok()?;
    Some(sign * (h * 60 + m))
}

fn parse_weekday(s: &str) -> Option<u8> {
    match s.trim().to_ascii_uppercase().as_str() {
        "SU" => Some(0),
        "MO" => Some(1),
        "TU" => Some(2),
        "WE" => Some(3),
        "TH" => Some(4),
        "FR" => Some(5),
        "SA" => Some(6),
        _ => None,
    }
}

fn parse_schedule(schedule: &str) -> Option<Parsed> {
    let mut once = None;
    let mut freq = None;
    let mut interval = 1u64;
    let mut at_min = None;
    let mut tz_off = 0i32;
    let mut byday = Vec::new();
    for part in schedule.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = part.split_once('=')?;
        let v = v.trim();
        match k.trim().to_ascii_uppercase().as_str() {
            "ONCE" | "AT_MS" => once = v.parse::<u64>().ok(),
            "FREQ" => freq = Some(v.to_ascii_uppercase()),
            "INTERVAL" => interval = v.parse::<u64>().ok()?.max(1),
            "AT" => at_min = parse_hhmm(v),
            "TZ" => tz_off = parse_tz(v).unwrap_or(0),
            "BYDAY" => byday = v.split(',').filter_map(parse_weekday).collect(),
            _ => {}
        }
    }
    if once.is_none() && freq.is_none() {
        return None;
    }
    Some(Parsed {
        once,
        freq,
        interval,
        at_min,
        tz_off,
        byday,
        period_ms: interval_ms(schedule),
    })
}

/// Next scheduled fire time (unix ms), strictly after the last run (or creation),
/// or None if there is no future occurrence (paused, one-shot already fired, or an
/// unparseable schedule). Supports one-shot (`ONCE=<ms>`), plain interval
/// (`FREQ=MINUTELY|HOURLY|DAILY;INTERVAL=N`), daily-at-time
/// (`FREQ=DAILY;INTERVAL=N;AT=HH:MM[;TZ=±HH:MM]`), and weekly-by-day
/// (`FREQ=WEEKLY;BYDAY=MO,WE,FR;AT=HH:MM[;TZ=±HH:MM]`). Pure integer time math —
/// no cron/chrono dependency; fixed TZ offsets only (no DST).
pub fn next_due(spec: &AutomationSpec, runs: &[AutomationRunSpec]) -> Option<u64> {
    if spec.status != "ACTIVE" {
        return None;
    }
    let p = parse_schedule(&spec.schedule)?;
    let last_run = runs
        .iter()
        .filter(|run| run.automation_id == spec.id)
        .map(|run| run.started_ms)
        .max();
    // One-shot fires exactly once, ever.
    if let Some(at) = p.once {
        return last_run.is_none().then_some(at);
    }
    let anchor = last_run.unwrap_or(spec.created_ms);
    // Plain interval (no time-of-day): next = last fire + period.
    if p.at_min.is_none() {
        return Some(anchor.saturating_add(p.period_ms?));
    }
    let at_min = p.at_min? as i64;
    let tz = p.tz_off as i64;
    let anchor_local = anchor as i64 + tz * MIN_MS;
    let mut day = anchor_local.div_euclid(DAY_MS);
    match p.freq.as_deref() {
        Some("DAILY") => {
            let created_day = (spec.created_ms as i64 + tz * MIN_MS).div_euclid(DAY_MS);
            let interval = p.interval.max(1) as i64;
            for _ in 0..(interval * 2 + 2) {
                let cand = day * DAY_MS + at_min * MIN_MS;
                if cand > anchor_local && (day - created_day).rem_euclid(interval) == 0 {
                    return Some((cand - tz * MIN_MS).max(0) as u64);
                }
                day += 1;
            }
            None
        }
        Some("WEEKLY") => {
            if p.byday.is_empty() {
                return None;
            }
            for _ in 0..15 {
                // 1970-01-01 was a Thursday; weekday 0=Sun..6=Sat.
                let weekday = ((day + 4).rem_euclid(7)) as u8;
                let cand = day * DAY_MS + at_min * MIN_MS;
                if cand > anchor_local && p.byday.contains(&weekday) {
                    return Some((cand - tz * MIN_MS).max(0) as u64);
                }
                day += 1;
            }
            None
        }
        _ => None,
    }
}

pub fn interval_ms(schedule: &str) -> Option<u64> {
    let mut freq = None;
    let mut interval = 1u64;
    for part in schedule.split(';') {
        let (key, value) = part.split_once('=')?;
        match key.trim().to_ascii_uppercase().as_str() {
            "FREQ" => freq = Some(value.trim().to_ascii_uppercase()),
            "INTERVAL" => interval = value.trim().parse::<u64>().ok()?,
            _ => {}
        }
    }
    let base: u64 = match freq.as_deref()? {
        "MINUTELY" => 60_000,
        "HOURLY" => 3_600_000,
        "DAILY" => 86_400_000,
        _ => return None,
    };
    base.checked_mul(interval.max(1))
}

pub fn latest_run<'a>(
    runs: &'a [AutomationRunSpec],
    automation_id: &str,
) -> Option<&'a AutomationRunSpec> {
    runs.iter()
        .filter(|run| run.automation_id == automation_id)
        .max_by_key(|run| run.started_ms)
}

pub fn id_from_name(name: &str, now_ms: u64) -> String {
    let slug = slug_fragment(name);
    let stem = if slug == "branch" {
        "automation"
    } else {
        slug.as_str()
    };
    format!("{stem}-{now_ms}")
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn slug_fragment(value: &str) -> String {
    let slug = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "branch".to_string()
    } else {
        slug
    }
}

// ── Run lifecycle ────────────────────────────────────────────────────────────

/// Canonical automation-run statuses. A run moves queued → running → done|failed;
/// `interrupted` is set by crash recovery for a run that was mid-flight when the
/// host died.
pub mod run_status {
    pub const QUEUED: &str = "queued";
    pub const RUNNING: &str = "running";
    pub const DONE: &str = "done";
    pub const FAILED: &str = "failed";
    pub const INTERRUPTED: &str = "interrupted";

    /// True for a status that is still in-flight (not a terminal outcome).
    pub fn is_active(status: &str) -> bool {
        status == QUEUED || status == RUNNING
    }
}

/// Update one run's status in place (persisted). No-op if the run id is unknown.
pub fn set_run_status(workspace: &Path, run_id: &str, status: &str) -> anyhow::Result<()> {
    let mut runs = read_runs(workspace)?;
    if let Some(run) = runs.iter_mut().find(|r| r.id == run_id) {
        run.status = status.to_string();
        write_run(workspace, run)?;
    }
    Ok(())
}

/// Crash recovery: any run still in an ACTIVE status (queued/running) from a
/// prior process that died is reconciled to `interrupted`, so the UI/scheduler
/// never shows a perpetually-"running" ghost. Returns the count reconciled. Call
/// once at scheduler/app startup.
pub fn reconcile_orphaned_runs(workspace: &Path) -> anyhow::Result<usize> {
    let runs = read_runs(workspace)?;
    let mut reconciled = 0usize;
    for mut run in runs {
        if run_status::is_active(&run.status) {
            run.status = run_status::INTERRUPTED.to_string();
            write_run(workspace, &run)?;
            reconciled += 1;
        }
    }
    Ok(reconciled)
}

// ── Scheduler ────────────────────────────────────────────────────────────────

/// All ACTIVE automations whose next fire time has arrived at `now_ms`.
pub fn due_automations<'a>(
    specs: &'a [AutomationSpec],
    runs: &[AutomationRunSpec],
    now_ms: u64,
) -> Vec<&'a AutomationSpec> {
    specs
        .iter()
        .filter(|spec| is_due(spec, runs, now_ms))
        .collect()
}

/// Soonest upcoming fire time across all specs (unix ms), or None if nothing is
/// scheduled — lets the scheduler sleep exactly until the next event instead of
/// busy-polling on a fixed cadence.
pub fn next_wakeup_ms(specs: &[AutomationSpec], runs: &[AutomationRunSpec]) -> Option<u64> {
    specs.iter().filter_map(|spec| next_due(spec, runs)).min()
}

/// Headless automation scheduler loop. Reconciles orphaned runs once, then on
/// each tick fires every due automation via `fire` and sleeps until the next one
/// is due (clamped to [min_tick, max_tick] so newly-added/edited specs are still
/// picked up promptly, and a stuck clock can't busy-spin).
///
/// `fire(spec)` is the host's hook to actually run the automation — it should
/// record a run (e.g. [`run_from_spec`] + [`write_run`] with status `queued`) and
/// dispatch the prompt to an engine; recording the run is what stops the next
/// tick from re-firing it. The loop owns NO agent logic and survives app close
/// ONLY while the host keeps this future alive (e.g. a background/tray process) —
/// that hosting is the remaining wiring.
pub async fn run_scheduler<F>(
    workspace: PathBuf,
    min_tick: Duration,
    max_tick: Duration,
    mut fire: F,
) where
    F: FnMut(&AutomationSpec),
{
    let _ = reconcile_orphaned_runs(&workspace);
    loop {
        let specs = read_specs(&workspace).unwrap_or_default();
        let runs = read_runs(&workspace).unwrap_or_default();
        let now = now_ms();
        for spec in due_automations(&specs, &runs, now) {
            fire(spec);
        }
        // Re-read after firing (a fire may have written a queued run) so the
        // wakeup reflects the new schedule horizon.
        let specs = read_specs(&workspace).unwrap_or_default();
        let runs = read_runs(&workspace).unwrap_or_default();
        let delay_ms = next_wakeup_ms(&specs, &runs)
            .map(|t| t.saturating_sub(now_ms()))
            .unwrap_or(u64::MAX)
            .clamp(min_tick.as_millis() as u64, max_tick.as_millis() as u64);
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn unique_tmp(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("oxide-{name}-{}", now_ms()));
        dir
    }

    fn spec() -> AutomationSpec {
        AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: "FREQ=DAILY;INTERVAL=1".to_string(),
            prompt: "Review the workspace".to_string(),
            created_ms: 10,
            script: None,
            webhook_token: None,
            session_id: None,
        }
    }

    #[test]
    fn specs_roundtrip_from_workspace_store() {
        let tmp = unique_tmp("automations");
        std::fs::create_dir_all(&tmp).unwrap();
        let spec = spec();

        write_spec(&tmp, &spec).unwrap();
        let specs = read_specs(&tmp).unwrap();

        assert_eq!(specs, vec![spec]);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn status_toggle_switches_active_and_paused() {
        let active = spec();

        let paused = with_toggled_status(&active);
        let active_again = with_toggled_status(&paused);

        assert_eq!(paused.status, "PAUSED");
        assert_eq!(active_again.status, "ACTIVE");
    }

    #[test]
    fn run_prompt_preserves_schedule_context() {
        let spec = spec();

        let prompt = build_run_prompt(&spec);

        assert!(prompt.contains("Run automation now"));
        assert!(prompt.contains("Daily review"));
        assert!(prompt.contains("Review the workspace"));
        assert!(prompt.contains("FREQ=DAILY"));
    }

    #[test]
    fn delete_spec_removes_toml_file() {
        let tmp = unique_tmp("automation-delete");
        std::fs::create_dir_all(&tmp).unwrap();
        let spec = spec();
        write_spec(&tmp, &spec).unwrap();

        delete_spec(&tmp, &spec.id).unwrap();

        assert!(read_specs(&tmp).unwrap().is_empty());
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn run_specs_roundtrip_from_workspace_store() {
        let tmp = unique_tmp("automation-runs");
        std::fs::create_dir_all(&tmp).unwrap();
        let run = AutomationRunSpec {
            id: "daily-review-20".to_string(),
            automation_id: "daily-review".to_string(),
            automation_name: "Daily review".to_string(),
            trigger: "manual".to_string(),
            status: "queued".to_string(),
            prompt: "Review the workspace".to_string(),
            started_ms: 20,
        };

        write_run(&tmp, &run).unwrap();
        let runs = read_runs(&tmp).unwrap();

        assert_eq!(runs, vec![run]);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn due_uses_schedule_interval_and_latest_run() {
        let spec = AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "ACTIVE".to_string(),
            schedule: "FREQ=MINUTELY;INTERVAL=5".to_string(),
            prompt: "Review the workspace".to_string(),
            created_ms: 1_000,
            script: None,
            webhook_token: None,
            session_id: None,
        };
        let recent_run = AutomationRunSpec {
            id: "daily-review-10000".to_string(),
            automation_id: spec.id.clone(),
            automation_name: spec.name.clone(),
            trigger: "scheduled".to_string(),
            status: "queued".to_string(),
            prompt: spec.prompt.clone(),
            started_ms: 10_000,
        };

        assert!(!is_due(&spec, &[], 120_000));
        assert!(is_due(&spec, &[], 310_000));
        assert!(!is_due(&spec, std::slice::from_ref(&recent_run), 250_000));
        assert!(is_due(&spec, &[recent_run], 311_000));
    }

    #[test]
    fn due_ignores_paused_or_invalid_schedules() {
        let mut spec = AutomationSpec {
            id: "daily-review".to_string(),
            name: "Daily review".to_string(),
            kind: "cron".to_string(),
            status: "PAUSED".to_string(),
            schedule: "FREQ=MINUTELY;INTERVAL=1".to_string(),
            prompt: "Review the workspace".to_string(),
            created_ms: 1,
            script: None,
            webhook_token: None,
            session_id: None,
        };

        assert!(!is_due(&spec, &[], 120_000));
        spec.status = "ACTIVE".to_string();
        spec.schedule = "bad schedule".to_string();
        assert!(!is_due(&spec, &[], 120_000));
    }

    #[test]
    fn id_from_name_uses_automation_fallback() {
        assert_eq!(id_from_name("!!!", 42), "automation-42");
        assert_eq!(id_from_name("Daily Review", 42), "daily-review-42");
    }

    #[test]
    fn once_fires_exactly_once() {
        let mut s = spec();
        s.created_ms = 0;
        s.schedule = "ONCE=500000".into();
        assert_eq!(next_due(&s, &[]), Some(500_000));
        assert!(is_due(&s, &[], 500_000));
        assert!(!is_due(&s, &[], 400_000));
        let run = run_from_spec(&s, "scheduled", "done", 500_000);
        assert_eq!(next_due(&s, &[run]), None);
    }

    #[test]
    fn daily_at_time_utc_advances_per_run() {
        let mut s = spec();
        s.created_ms = 0; // 1970-01-01 00:00 UTC
        s.schedule = "FREQ=DAILY;AT=09:00".into();
        // first occurrence = 09:00 on day 0
        assert_eq!(next_due(&s, &[]), Some(32_400_000));
        // after firing at day0 09:00, next is day1 09:00
        let run = run_from_spec(&s, "scheduled", "done", 32_400_000);
        assert_eq!(next_due(&s, &[run]), Some(32_400_000 + 86_400_000));
    }

    #[test]
    fn daily_at_time_honors_tz_offset() {
        let mut s = spec();
        s.created_ms = 0;
        // 09:00 at +07:00 == 02:00 UTC == 7_200_000 ms
        s.schedule = "FREQ=DAILY;AT=09:00;TZ=+07:00".into();
        assert_eq!(next_due(&s, &[]), Some(7_200_000));
    }

    #[test]
    fn weekly_by_day_finds_next_matching_weekday() {
        let mut s = spec();
        s.created_ms = 0; // Thursday
        s.schedule = "FREQ=WEEKLY;BYDAY=MO;AT=00:00".into();
        // next Monday after day-0 Thursday is day 4
        assert_eq!(next_due(&s, &[]), Some(4 * 86_400_000));
    }

    #[test]
    fn interval_schedule_still_works_via_next_due() {
        let s = AutomationSpec {
            schedule: "FREQ=MINUTELY;INTERVAL=5".to_string(),
            created_ms: 1_000,
            ..spec()
        };
        assert_eq!(next_due(&s, &[]), Some(301_000));
        assert!(is_due(&s, &[], 310_000));
        assert!(!is_due(&s, &[], 120_000));
    }

    #[test]
    fn reconcile_marks_only_active_runs_interrupted() {
        let tmp = unique_tmp("reconcile");
        std::fs::create_dir_all(&tmp).unwrap();
        let running = AutomationRunSpec {
            id: "r1".into(),
            automation_id: "a".into(),
            automation_name: "A".into(),
            trigger: "scheduled".into(),
            status: run_status::RUNNING.into(),
            prompt: "p".into(),
            started_ms: 1,
        };
        let done = AutomationRunSpec {
            id: "r2".into(),
            status: run_status::DONE.into(),
            started_ms: 2,
            ..running.clone()
        };
        write_run(&tmp, &running).unwrap();
        write_run(&tmp, &done).unwrap();

        assert_eq!(reconcile_orphaned_runs(&tmp).unwrap(), 1);

        let runs = read_runs(&tmp).unwrap();
        let r1 = runs.iter().find(|r| r.id == "r1").unwrap();
        let r2 = runs.iter().find(|r| r.id == "r2").unwrap();
        assert_eq!(r1.status, run_status::INTERRUPTED);
        assert_eq!(r2.status, run_status::DONE);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn due_batch_and_next_wakeup() {
        let s1 = AutomationSpec {
            id: "s1".into(),
            schedule: "FREQ=MINUTELY;INTERVAL=5".into(),
            created_ms: 0,
            ..spec()
        };
        let s2 = AutomationSpec {
            id: "s2".into(),
            schedule: "ONCE=100000".into(),
            created_ms: 0,
            ..spec()
        };
        let specs = vec![s1, s2];
        // now=0: s1 due at 300000, s2 at 100000 → nothing due, soonest is 100000
        assert!(due_automations(&specs, &[], 0).is_empty());
        assert_eq!(next_wakeup_ms(&specs, &[]), Some(100_000));
        // now past both
        assert_eq!(due_automations(&specs, &[], 300_001).len(), 2);
    }
}
