use std::path::Path;

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
    let path = workspace.join(".oxide/automations").join(format!("{id}.toml"));
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
        "Run automation now\n\nName: {}\nKind: {}\nSchedule: {}\nStatus: {}\n\nAutomation prompt:\n{}",
        spec.name, spec.kind, spec.schedule, spec.status, spec.prompt
    )
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
    if spec.status != "ACTIVE" {
        return false;
    }
    let Some(interval_ms) = interval_ms(&spec.schedule) else {
        return false;
    };
    let last_run = runs
        .iter()
        .filter(|run| run.automation_id == spec.id)
        .map(|run| run.started_ms)
        .max();
    let anchor = last_run.unwrap_or(spec.created_ms);
    now_ms >= anchor.saturating_add(interval_ms)
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
    let stem = if slug == "branch" { "automation" } else { slug.as_str() };
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
        assert!(!is_due(&spec, &[recent_run.clone()], 250_000));
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
}
