//! Rust-native design contracts for Oxide.
//!
//! This crate intentionally has no UI or runtime dependency. It models the
//! Open Design-style pieces Oxide needs: `DESIGN.md`, token contracts, element
//! selections, and small deterministic visual review checks.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const REQUIRED_SECTION_COUNT: usize = 9;

const TOKEN_SCHEMA: &[TokenSpec] = &[
    TokenSpec::required(
        "--bg",
        TokenLayer::Identity,
        &["bg", "background", "canvas"],
        "#f8fafc",
    ),
    TokenSpec::required(
        "--surface",
        TokenLayer::Identity,
        &["surface", "card", "panel"],
        "#ffffff",
    ),
    TokenSpec::required(
        "--fg",
        TokenLayer::Identity,
        &["fg", "foreground", "text"],
        "#111827",
    ),
    TokenSpec::required(
        "--muted",
        TokenLayer::Identity,
        &["muted", "subtext", "caption"],
        "#6b7280",
    ),
    TokenSpec::required(
        "--border",
        TokenLayer::Identity,
        &["border", "separator", "stroke"],
        "#d1d5db",
    ),
    TokenSpec::required(
        "--accent",
        TokenLayer::Identity,
        &["accent", "primary", "brand"],
        "#2563eb",
    ),
    TokenSpec::required(
        "--font-display",
        TokenLayer::Structure,
        &["font-display", "font-heading", "display", "heading"],
        "Inter, ui-sans-serif, system-ui, sans-serif",
    ),
    TokenSpec::required(
        "--font-body",
        TokenLayer::Structure,
        &["font-body", "font-sans", "body", "font"],
        "Inter, ui-sans-serif, system-ui, sans-serif",
    ),
    TokenSpec::required(
        "--text-base",
        TokenLayer::Structure,
        &["text-base", "body-size"],
        "1rem",
    ),
    TokenSpec::fallback("--space-4", &["space-4", "spacing", "gap"], "16px"),
    TokenSpec::fallback("--radius-md", &["radius-md", "radius", "corner"], "8px"),
    TokenSpec::fallback("--motion-fast", &["motion-fast", "duration-fast"], "150ms"),
    TokenSpec::fallback(
        "--motion-base",
        &["motion-base", "duration", "transition"],
        "200ms",
    ),
    TokenSpec::fallback(
        "--ease-standard",
        &["ease-standard", "easing", "ease"],
        "cubic-bezier(0.2, 0, 0, 1)",
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignSystem {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default)]
    pub sections: BTreeMap<u8, DesignSection>,
    #[serde(default)]
    pub missing_sections: Vec<u8>,
}

impl DesignSystem {
    pub fn is_complete(&self) -> bool {
        self.missing_sections.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignSection {
    pub number: u8,
    pub heading: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDesignToken {
    pub name: String,
    pub value: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignTokenContract {
    pub schema_version: u8,
    pub summary: DesignTokenSummary,
    pub tokens: Vec<DesignTokenBinding>,
    pub tokens_css: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignTokenSummary {
    pub total_tokens: usize,
    pub source_backed_tokens: usize,
    pub fallback_tokens: usize,
    pub grade: DesignTokenGrade,
    pub recommend_rebuild: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesignTokenGrade {
    Excellent,
    Usable,
    NeedsReview,
    NeedsRebuild,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignTokenBinding {
    pub name: String,
    pub layer: TokenLayer,
    pub value: String,
    pub confidence: TokenConfidence,
    pub reason: String,
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenLayer {
    Identity,
    Structure,
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenConfidence {
    High,
    Medium,
    Fallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DesignSelection {
    pub selector: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub component: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub html: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub styles: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignEdit {
    pub property: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub old_value: String,
    pub new_value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignReviewInput {
    pub selection: DesignSelection,
    #[serde(default)]
    pub edits: Vec<DesignEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignReview {
    pub ok: bool,
    pub score: u8,
    pub findings: Vec<DesignFinding>,
    pub checklist: Vec<DesignChecklistItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignFinding {
    pub severity: DesignSeverity,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignChecklistItem {
    pub label: String,
    pub passed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignPatchProposal {
    pub selection: DesignSelection,
    pub edits: Vec<DesignEdit>,
    #[serde(default)]
    pub instruction: String,
}

#[derive(Debug, Clone, Copy)]
struct TokenSpec {
    name: &'static str,
    layer: TokenLayer,
    hints: &'static [&'static str],
    fallback: &'static str,
    source_required: bool,
}

impl TokenSpec {
    const fn required(
        name: &'static str,
        layer: TokenLayer,
        hints: &'static [&'static str],
        fallback: &'static str,
    ) -> Self {
        Self {
            name,
            layer,
            hints,
            fallback,
            source_required: true,
        }
    }

    const fn fallback(
        name: &'static str,
        hints: &'static [&'static str],
        fallback: &'static str,
    ) -> Self {
        Self {
            name,
            layer: TokenLayer::Fallback,
            hints,
            fallback,
            source_required: false,
        }
    }
}

pub fn parse_design_markdown(input: &str) -> DesignSystem {
    let title = input
        .lines()
        .find_map(|line| line.trim().strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .unwrap_or("Untitled Design System")
        .to_string();
    let category = input.lines().find_map(|line| {
        line.trim()
            .strip_prefix("> Category:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    });

    let mut sections: BTreeMap<u8, DesignSection> = BTreeMap::new();
    let mut current: Option<DesignSection> = None;
    for line in input.lines() {
        if let Some((number, heading)) = parse_numbered_heading(line) {
            if let Some(section) = current.take() {
                sections.insert(section.number, section);
            }
            current = Some(DesignSection {
                number,
                heading,
                body: String::new(),
            });
            continue;
        }
        if let Some(section) = current.as_mut() {
            if !section.body.is_empty() {
                section.body.push('\n');
            }
            section.body.push_str(line);
        }
    }
    if let Some(section) = current {
        sections.insert(section.number, section);
    }

    let missing_sections = (1..=REQUIRED_SECTION_COUNT as u8)
        .filter(|number| !sections.contains_key(number))
        .collect();
    DesignSystem {
        title,
        category,
        sections,
        missing_sections,
    }
}

pub fn extract_source_tokens(input: &str, source: &str) -> Vec<SourceDesignToken> {
    let mut tokens = Vec::new();
    let mut seen = BTreeSet::new();
    for (idx, line) in input.lines().enumerate() {
        if let Some((name, value)) = parse_css_custom_property(line) {
            if seen.insert(name.clone()) {
                tokens.push(SourceDesignToken {
                    name,
                    value,
                    source: source.to_string(),
                    line: Some(idx + 1),
                });
            }
        }
    }
    tokens
}

pub fn build_design_token_contract(source_tokens: &[SourceDesignToken]) -> DesignTokenContract {
    let bindings: Vec<DesignTokenBinding> = TOKEN_SCHEMA
        .iter()
        .map(|spec| bind_token(*spec, source_tokens))
        .collect();
    let source_backed_tokens = bindings
        .iter()
        .filter(|binding| binding.confidence != TokenConfidence::Fallback)
        .count();
    let fallback_tokens = bindings.len().saturating_sub(source_backed_tokens);
    let required_backed = bindings
        .iter()
        .filter(|binding| binding.layer != TokenLayer::Fallback)
        .filter(|binding| binding.confidence != TokenConfidence::Fallback)
        .count();
    let required_total = TOKEN_SCHEMA
        .iter()
        .filter(|spec| spec.source_required)
        .count();
    let grade = if source_backed_tokens == bindings.len() {
        DesignTokenGrade::Excellent
    } else if required_backed == required_total {
        DesignTokenGrade::Usable
    } else if required_backed >= required_total / 2 {
        DesignTokenGrade::NeedsReview
    } else {
        DesignTokenGrade::NeedsRebuild
    };
    let tokens_css = render_tokens_css(&bindings);
    DesignTokenContract {
        schema_version: 1,
        summary: DesignTokenSummary {
            total_tokens: bindings.len(),
            source_backed_tokens,
            fallback_tokens,
            grade,
            recommend_rebuild: matches!(
                grade,
                DesignTokenGrade::NeedsReview | DesignTokenGrade::NeedsRebuild
            ),
        },
        tokens: bindings,
        tokens_css,
    }
}

pub fn review_design_selection(input: DesignReviewInput) -> DesignReview {
    let mut findings = Vec::new();
    if input.selection.selector.trim().is_empty() {
        findings.push(error("Missing selector", "Design edits need a stable selector or component hint before they can be applied safely."));
    }
    if input.edits.is_empty() {
        findings.push(warning("No edits recorded", "Select an element and change at least one property before asking Oxide to apply code changes."));
    }
    for edit in &input.edits {
        let prop = edit.property.trim();
        let value = edit.new_value.trim();
        if (prop.eq_ignore_ascii_case("color") || prop.eq_ignore_ascii_case("background"))
            && is_raw_color(value)
        {
            findings.push(warning(
                "Raw color edit",
                "Prefer an existing design token or CSS variable over hard-coded color values.",
            ));
        }
        if (prop.contains("duration") || prop.contains("motion") || prop == "transition")
            && duration_ms(value).is_some_and(|ms| ms > 500)
        {
            findings.push(warning(
                "Long transition",
                "Non-navigation microinteractions should stay at or below 500ms.",
            ));
        }
        if prop == "transform" && value.contains("scale(") {
            findings.push(DesignFinding {
                severity: DesignSeverity::Info,
                title: "Reduced-motion check".to_string(),
                detail: "Scale/transform animation should have a prefers-reduced-motion fallback before production.".to_string(),
            });
        }
    }
    let ok = !findings
        .iter()
        .any(|finding| finding.severity == DesignSeverity::Error);
    let warnings = findings
        .iter()
        .filter(|finding| finding.severity == DesignSeverity::Warning)
        .count() as u8;
    let score = if ok {
        100u8.saturating_sub(warnings * 12)
    } else {
        40
    };
    let checklist = vec![
        DesignChecklistItem {
            label: "Stable selector captured".to_string(),
            passed: !input.selection.selector.trim().is_empty(),
        },
        DesignChecklistItem {
            label: "At least one edit recorded".to_string(),
            passed: !input.edits.is_empty(),
        },
        DesignChecklistItem {
            label: "No blocking review errors".to_string(),
            passed: ok,
        },
    ];
    DesignReview {
        ok,
        score,
        findings,
        checklist,
    }
}

pub fn build_patch_instruction(proposal: &DesignPatchProposal) -> String {
    let mut out = String::from("Apply these Design Workbench edits to the source code.\n");
    out.push_str("- Preserve existing design tokens/classes when possible.\n");
    out.push_str("- Avoid hard-coded colors unless the local design system already uses them.\n");
    out.push_str(
        "- Keep motion under 500ms and respect reduced motion for transform-based changes.\n\n",
    );
    out.push_str(&format!("- selector: {}\n", proposal.selection.selector));
    if !proposal.selection.component.is_empty() {
        out.push_str(&format!(
            "- component: <{}>\n",
            proposal.selection.component
        ));
    }
    if !proposal.selection.source.is_empty() {
        out.push_str(&format!("- source: {}\n", proposal.selection.source));
    }
    if !proposal.selection.html.is_empty() {
        out.push_str(&format!("- html: {}\n", proposal.selection.html));
    }
    out.push_str("- edits:\n");
    for edit in &proposal.edits {
        out.push_str(&format!(
            "  - {}: {} -> {}\n",
            edit.property, edit.old_value, edit.new_value
        ));
    }
    if !proposal.instruction.trim().is_empty() {
        out.push_str("\nAdditional instruction:\n");
        out.push_str(proposal.instruction.trim());
        out.push('\n');
    }
    out
}

fn parse_numbered_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("## ")?;
    let (number, heading) = rest.split_once('.')?;
    let number = number.trim().parse::<u8>().ok()?;
    if !(1..=REQUIRED_SECTION_COUNT as u8).contains(&number) {
        return None;
    }
    Some((number, heading.trim().to_string()))
}

fn parse_css_custom_property(line: &str) -> Option<(String, String)> {
    let start = line.find("--")?;
    let rest = &line[start..];
    let (name, value) = rest.split_once(':')?;
    let name = name.trim();
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return None;
    }
    let value = value
        .split(';')
        .next()
        .unwrap_or(value)
        .trim()
        .trim_matches('`')
        .to_string();
    if value.is_empty() {
        return None;
    }
    Some((name.to_string(), value))
}

fn bind_token(spec: TokenSpec, source_tokens: &[SourceDesignToken]) -> DesignTokenBinding {
    if let Some(token) = source_tokens.iter().find(|token| token.name == spec.name) {
        return DesignTokenBinding {
            name: spec.name.to_string(),
            layer: spec.layer,
            value: token.value.clone(),
            confidence: TokenConfidence::High,
            reason: format!("Exact source token {} matched.", token.name),
            sources: vec![source_ref(token)],
        };
    }
    let candidate = source_tokens.iter().find(|token| {
        let name = token.name.trim_start_matches("--").to_ascii_lowercase();
        spec.hints.iter().any(|hint| name.contains(hint))
    });
    if let Some(token) = candidate {
        return DesignTokenBinding {
            name: spec.name.to_string(),
            layer: spec.layer,
            value: token.value.clone(),
            confidence: TokenConfidence::Medium,
            reason: format!("Mapped from related source token {}.", token.name),
            sources: vec![source_ref(token)],
        };
    }
    DesignTokenBinding {
        name: spec.name.to_string(),
        layer: spec.layer,
        value: spec.fallback.to_string(),
        confidence: TokenConfidence::Fallback,
        reason: "No matching source token found; using Oxide fallback.".to_string(),
        sources: Vec::new(),
    }
}

fn render_tokens_css(bindings: &[DesignTokenBinding]) -> String {
    let mut out = String::from(":root {\n");
    for binding in bindings {
        out.push_str(&format!("  {}: {};\n", binding.name, binding.value));
    }
    out.push_str("}\n");
    out
}

fn source_ref(token: &SourceDesignToken) -> String {
    match token.line {
        Some(line) => format!("{}:{line}", token.source),
        None => token.source.clone(),
    }
}

fn error(title: &str, detail: &str) -> DesignFinding {
    DesignFinding {
        severity: DesignSeverity::Error,
        title: title.to_string(),
        detail: detail.to_string(),
    }
}

fn warning(title: &str, detail: &str) -> DesignFinding {
    DesignFinding {
        severity: DesignSeverity::Warning,
        title: title.to_string(),
        detail: detail.to_string(),
    }
}

fn is_raw_color(value: &str) -> bool {
    value.starts_with('#') || value.starts_with("rgb(") || value.starts_with("rgba(")
}

fn duration_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Some(raw) = value.strip_suffix("ms") {
        return raw.trim().parse::<u64>().ok();
    }
    if let Some(raw) = value.strip_suffix('s') {
        let seconds = raw.trim().parse::<f64>().ok()?;
        return Some((seconds * 1000.0).round() as u64);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_design_markdown_sections() {
        let mut input = String::from("# Acme\n> Category: SaaS\n\n");
        for n in 1..=9 {
            input.push_str(&format!("## {n}. Section {n}\nbody {n}\n"));
        }
        let parsed = parse_design_markdown(&input);
        assert_eq!(parsed.title, "Acme");
        assert_eq!(parsed.category.as_deref(), Some("SaaS"));
        assert!(parsed.is_complete());
        assert_eq!(parsed.sections.len(), 9);
    }

    #[test]
    fn builds_token_contract_from_css_vars() {
        let input = ":root {\n  --bg: #000;\n  --accent: #ff5500;\n  --font-body: Inter;\n}";
        let tokens = extract_source_tokens(input, "DESIGN.md");
        let contract = build_design_token_contract(&tokens);
        assert!(contract.summary.source_backed_tokens >= 3);
        assert!(contract.tokens_css.contains("--accent: #ff5500;"));
    }

    #[test]
    fn review_flags_missing_selector_and_raw_color() {
        let review = review_design_selection(DesignReviewInput {
            selection: DesignSelection::default(),
            edits: vec![DesignEdit {
                property: "color".to_string(),
                old_value: String::new(),
                new_value: "#ffffff".to_string(),
            }],
        });
        assert!(!review.ok);
        assert!(review
            .findings
            .iter()
            .any(|finding| finding.severity == DesignSeverity::Error));
        assert!(review
            .findings
            .iter()
            .any(|finding| finding.title == "Raw color edit"));
    }
}
