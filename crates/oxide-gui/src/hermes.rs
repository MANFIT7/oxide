use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HermesProfile {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub validation: String,
    pub review_prompt: String,
    pub created_ms: u64,
}

pub fn read_profiles(workspace: &Path) -> anyhow::Result<Vec<HermesProfile>> {
    let dir = workspace.join(".oxide/hermes-profiles");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        profiles.push(toml::from_str::<HermesProfile>(&text)?);
    }
    profiles.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(profiles)
}

pub fn write_profile(workspace: &Path, profile: &HermesProfile) -> anyhow::Result<()> {
    let dir = workspace.join(".oxide/hermes-profiles");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", profile.id));
    let text = toml::to_string_pretty(profile)?;
    std::fs::write(path, text)?;
    Ok(())
}

pub fn delete_profile(workspace: &Path, id: &str) -> anyhow::Result<()> {
    let path = workspace
        .join(".oxide/hermes-profiles")
        .join(format!("{id}.toml"));
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn profile_from_fields(
    name: &str,
    goal: &str,
    validation: &str,
    review_prompt: &str,
    created_ms: u64,
) -> anyhow::Result<HermesProfile> {
    let name = name.trim();
    let goal = goal.trim();
    let validation = validation.trim();
    if name.is_empty() {
        anyhow::bail!("Hermes profile name is required");
    }
    if goal.is_empty() {
        anyhow::bail!("Hermes profile goal is required");
    }
    if validation.is_empty() {
        anyhow::bail!("Hermes profile validation command is required");
    }
    Ok(HermesProfile {
        id: format!("{}-{created_ms}", slug_fragment(name)),
        name: name.to_string(),
        goal: goal.to_string(),
        validation: validation.to_string(),
        review_prompt: review_prompt.trim().to_string(),
        created_ms,
    })
}

pub fn build_evolve_prompt(goal: &str, validation: &str, diff_context: &str) -> String {
    format!(
        "Hermes evolve\n\nGoal:\n{goal}\n\nValidation command(s):\n{validation}\n\nCurrent workspace diff/status context:\n{diff_context}\n\nInstructions:\n1. Inspect the current repository state.\n2. Propose the smallest high-impact improvement toward the goal.\n3. Implement it with focused edits.\n4. Run the validation command(s) when practical.\n5. Report changed files, validation result, and remaining risks."
    )
}

pub fn build_review_prompt(goal: &str, validation: &str, review_prompt: &str) -> String {
    format!(
        "Hermes review loop\n\nGoal:\n{goal}\n\nValidation command(s):\n{validation}\n\nReview gate:\n{review_prompt}\n\nInstructions:\n1. Review the latest workspace changes against the goal.\n2. Identify spec gaps, code quality risks, UX regressions, and missing validation.\n3. Fix concrete issues when practical.\n4. Re-run validation when practical.\n5. Report what changed, what passed, and what remains risky."
    )
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
        "hermes".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_from_fields_requires_goal_and_validation() {
        assert!(profile_from_fields("Lane", "", "cargo test", "", 1).is_err());
        assert!(profile_from_fields("Lane", "Improve agents", "", "", 1).is_err());
    }

    #[test]
    fn profile_from_fields_builds_stable_slug() {
        let profile =
            profile_from_fields("Hermes Lane!", "Improve agents", "cargo test", "DONE", 42)
                .unwrap();
        assert_eq!(profile.id, "hermes-lane-42");
        assert_eq!(profile.review_prompt, "DONE");
    }

    #[test]
    fn prompts_include_validation_and_context() {
        let prompt = build_evolve_prompt("Goal", "cargo check", "git status");
        assert!(prompt.contains("Hermes evolve"));
        assert!(prompt.contains("cargo check"));
        assert!(prompt.contains("git status"));

        let review = build_review_prompt("Goal", "cargo test", "No gaps");
        assert!(review.contains("Hermes review loop"));
        assert!(review.contains("No gaps"));
    }
}
