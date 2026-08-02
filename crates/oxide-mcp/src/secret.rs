use serde_json::Value;

const REDACTED: &str = "[REDACTED]";
const SENSITIVE_JSON_KEYS: &[&str] = &[
    "accesstoken",
    "apikey",
    "authorization",
    "clientsecret",
    "code",
    "codeverifier",
    "idtoken",
    "password",
    "pkceverifier",
    "privatekey",
    "refreshtoken",
    "secret",
    "state",
    "token",
    "xapikey",
];

/// A secret that cannot be exposed accidentally through `Debug` or `Display`.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Expose the secret only at the point where it is sent to its destination.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

impl std::fmt::Display for SecretString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(REDACTED)
    }
}

/// Redact common OAuth material before provider responses reach logs or errors.
pub fn redact_oauth_secrets(input: &str) -> String {
    if let Ok(mut json) = serde_json::from_str::<Value>(input) {
        redact_json_value(&mut json);
        return serde_json::to_string(&json).unwrap_or_else(|_| REDACTED.to_string());
    }

    redact_unstructured(input)
}

fn redact_unstructured(input: &str) -> String {
    let mut redacted = input.to_string();
    for marker in [
        "bearer ",
        "access_token=",
        "refresh_token=",
        "id_token=",
        "client_secret=",
        "code=",
        "state=",
    ] {
        redacted = redact_values_after(&redacted, marker);
    }
    redacted
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if is_sensitive_json_key(key) {
                    *value = Value::String(REDACTED.to_string());
                } else {
                    redact_json_value(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_json_value),
        Value::String(text) => *text = redact_unstructured(text),
        _ => {}
    }
}

fn is_sensitive_json_key(key: &str) -> bool {
    let canonical = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    SENSITIVE_JSON_KEYS.contains(&canonical.as_str())
}

fn redact_values_after(input: &str, marker: &str) -> String {
    let mut output = input.to_string();
    let marker = marker.to_ascii_lowercase();
    let mut search_from = 0;

    loop {
        let lowercase = output.to_ascii_lowercase();
        let Some(relative_start) = lowercase[search_from..].find(&marker) else {
            break;
        };
        let value_start = search_from + relative_start + marker.len();
        let value_end = output[value_start..]
            .find(|character: char| {
                character.is_whitespace()
                    || matches!(character, '&' | ',' | ';' | '"' | '\'' | '<' | '>')
            })
            .map(|offset| value_start + offset)
            .unwrap_or(output.len());
        output.replace_range(value_start..value_end, REDACTED);
        search_from = value_start + REDACTED.len();
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_and_display_are_redacted() {
        let secret = SecretString::new("do-not-print");

        assert_eq!(format!("{secret}"), REDACTED);
        assert!(!format!("{secret:?}").contains("do-not-print"));
    }

    #[test]
    fn redacts_json_and_header_tokens() {
        let json = redact_oauth_secrets(
            r#"{"access_token":"access-secret","nested":{"refresh_token":"refresh-secret"},"detail":"Authorization: Bearer embedded-secret","error":"invalid_grant"}"#,
        );
        let header = redact_oauth_secrets("Authorization: Bearer header-secret");
        let camel_case = redact_oauth_secrets(
            r#"{"accessToken":"access-secret","refreshToken":"refresh-secret","clientSecret":"client-secret"}"#,
        );

        assert!(!json.contains("access-secret"));
        assert!(!json.contains("refresh-secret"));
        assert!(!json.contains("embedded-secret"));
        assert!(json.contains("invalid_grant"));
        assert!(!header.contains("header-secret"));
        assert!(!camel_case.contains("access-secret"));
        assert!(!camel_case.contains("refresh-secret"));
        assert!(!camel_case.contains("client-secret"));
    }

    #[test]
    fn redacts_common_provider_secret_keys_and_variants() {
        let redacted = redact_oauth_secrets(
            r#"{
                "api_key":"api-secret",
                "apiKey":"camel-api-secret",
                "password":"password-secret",
                "secret":"generic-secret",
                "private_key":"private-key-secret",
                "privateKey":"camel-private-key-secret",
                "x-api-key":"header-api-secret"
            }"#,
        );

        for secret in [
            "api-secret",
            "camel-api-secret",
            "password-secret",
            "generic-secret",
            "private-key-secret",
            "camel-private-key-secret",
            "header-api-secret",
        ] {
            assert!(!redacted.contains(secret));
        }
    }
}
