use std::collections::BTreeSet;

const SUPABASE_MCP_ENDPOINT: &str = "https://mcp.supabase.com/mcp";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SupabaseFeature {
    Account,
    Docs,
    Database,
    Debugging,
    Development,
    Functions,
    Branching,
    Storage,
}

impl SupabaseFeature {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Docs => "docs",
            Self::Database => "database",
            Self::Debugging => "debugging",
            Self::Development => "development",
            Self::Functions => "functions",
            Self::Branching => "branching",
            Self::Storage => "storage",
        }
    }
}

impl std::str::FromStr for SupabaseFeature {
    type Err = SupabasePresetError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "account" => Ok(Self::Account),
            "docs" => Ok(Self::Docs),
            "database" => Ok(Self::Database),
            "debugging" => Ok(Self::Debugging),
            "development" => Ok(Self::Development),
            "functions" => Ok(Self::Functions),
            "branching" => Ok(Self::Branching),
            "storage" => Ok(Self::Storage),
            value => Err(SupabasePresetError::UnsupportedFeature(value.to_string())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupabaseMcpPreset {
    project_ref: String,
    read_only: bool,
    features: Vec<SupabaseFeature>,
    endpoint: String,
}

impl SupabaseMcpPreset {
    pub fn project_ref(&self) -> &str {
        &self.project_ref
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn features(&self) -> &[SupabaseFeature] {
        &self.features
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Stable profile key for isolating OAuth credentials between projects.
    pub fn credential_profile_id(&self) -> String {
        if self.project_ref.is_empty() {
            "supabase:account".to_string()
        } else {
            format!("supabase:{}", self.project_ref)
        }
    }
}

#[derive(Clone, Debug)]
pub struct SupabasePresetBuilder {
    project_ref: String,
    read_only: bool,
    features: BTreeSet<SupabaseFeature>,
}

impl SupabasePresetBuilder {
    pub fn for_account() -> Self {
        Self {
            project_ref: String::new(),
            read_only: true,
            features: BTreeSet::from([
                SupabaseFeature::Account,
                SupabaseFeature::Docs,
                SupabaseFeature::Database,
            ]),
        }
    }

    pub fn new(project_ref: impl Into<String>) -> Result<Self, SupabasePresetError> {
        let project_ref = project_ref.into();
        validate_project_ref(&project_ref)?;
        Ok(Self {
            project_ref,
            read_only: true,
            features: BTreeSet::from([
                SupabaseFeature::Docs,
                SupabaseFeature::Database,
                SupabaseFeature::Debugging,
            ]),
        })
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn features<I>(mut self, features: I) -> Result<Self, SupabasePresetError>
    where
        I: IntoIterator<Item = SupabaseFeature>,
    {
        let features = features.into_iter().collect::<BTreeSet<_>>();
        if features.is_empty() {
            return Err(SupabasePresetError::EmptyFeatures);
        }
        self.features = features;
        Ok(self)
    }

    pub fn feature_names<I, S>(self, features: I) -> Result<Self, SupabasePresetError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let parsed = features
            .into_iter()
            .map(|feature| feature.as_ref().parse())
            .collect::<Result<Vec<_>, _>>()?;
        self.features(parsed)
    }

    pub fn build(self) -> SupabaseMcpPreset {
        let features = self.features.into_iter().collect::<Vec<_>>();
        let feature_names = features
            .iter()
            .map(|feature| feature.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let mut endpoint = reqwest::Url::parse(SUPABASE_MCP_ENDPOINT)
            .expect("the built-in Supabase MCP endpoint is valid");
        let mut query = endpoint.query_pairs_mut();
        if !self.project_ref.is_empty() {
            query.append_pair("project_ref", &self.project_ref);
        }
        query
            .append_pair("read_only", if self.read_only { "true" } else { "false" })
            .append_pair("features", &feature_names);
        drop(query);
        SupabaseMcpPreset {
            project_ref: self.project_ref,
            read_only: self.read_only,
            features,
            endpoint: endpoint.into(),
        }
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum SupabasePresetError {
    #[error("Supabase MCP project_ref is required")]
    MissingProjectRef,
    #[error("invalid Supabase MCP project_ref")]
    InvalidProjectRef,
    #[error("Supabase MCP feature list cannot be empty")]
    EmptyFeatures,
    #[error("unsupported Supabase MCP feature: {0}")]
    UnsupportedFeature(String),
}

fn validate_project_ref(project_ref: &str) -> Result<(), SupabasePresetError> {
    if project_ref.trim().is_empty() {
        return Err(SupabasePresetError::MissingProjectRef);
    }
    if project_ref.len() > 128
        || !project_ref
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SupabasePresetError::InvalidProjectRef);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_is_project_scoped_and_read_only_by_default() {
        let preset = SupabasePresetBuilder::new("abc123").unwrap().build();
        let url = reqwest::Url::parse(preset.endpoint()).unwrap();
        let query = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();

        assert_eq!(
            query.get("project_ref").map(|value| value.as_ref()),
            Some("abc123")
        );
        assert_eq!(
            query.get("read_only").map(|value| value.as_ref()),
            Some("true")
        );
        assert_eq!(preset.credential_profile_id(), "supabase:abc123");
    }

    #[test]
    fn account_preset_exposes_project_selection_without_a_project_ref() {
        let preset = SupabasePresetBuilder::for_account().build();
        let url = reqwest::Url::parse(preset.endpoint()).unwrap();
        let query = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();

        assert!(!query.contains_key("project_ref"));
        assert_eq!(
            query.get("read_only").map(|value| value.as_ref()),
            Some("true")
        );
        assert!(preset.features().contains(&SupabaseFeature::Account));
        assert_eq!(preset.credential_profile_id(), "supabase:account");
    }

    #[test]
    fn rejects_missing_project_and_unknown_features() {
        assert_eq!(
            SupabasePresetBuilder::new(" ").unwrap_err(),
            SupabasePresetError::MissingProjectRef
        );
        assert_eq!(
            SupabasePresetBuilder::new("abc123")
                .unwrap()
                .feature_names(["database", "not-real"])
                .unwrap_err(),
            SupabasePresetError::UnsupportedFeature("not-real".to_string())
        );
    }

    #[test]
    fn feature_allowlist_is_encoded_without_duplicates() {
        let preset = SupabasePresetBuilder::new("abc123")
            .unwrap()
            .features([
                SupabaseFeature::Database,
                SupabaseFeature::Docs,
                SupabaseFeature::Database,
            ])
            .unwrap()
            .build();

        assert_eq!(preset.features().len(), 2);
        assert!(preset.endpoint().contains("features=docs%2Cdatabase"));
    }
}
