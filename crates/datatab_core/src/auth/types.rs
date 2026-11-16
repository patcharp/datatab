use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use secrecy::SecretString;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Top-level capability descriptor for an auth provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthProviderCapabilities {
    #[serde(default)]
    pub login: AuthProviderLoginCapabilities,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthProviderLoginCapabilities {
    #[serde(default)]
    pub supported: bool,
    #[serde(default)]
    pub verification_url_progress: bool,
}

/// `AuthProfile` uses a custom `Deserialize` impl to handle migration from
/// the legacy `"config"` format (see impl below). `Serialize` is derived
/// normally and always writes the new `"fields"` format.
#[derive(Clone, Serialize)]
pub struct AuthProfile {
    pub id: Uuid,
    pub name: String,
    pub provider_id: String,
    pub fields: HashMap<String, String>,
    /// Secret-kind field values (`Password` / `WriteOnly`).
    ///
    /// Held as `SecretString` so the value is zeroized on drop and never leaks
    /// through `Debug` or serde output. These are persisted to the OS keyring,
    /// never to SQLite in plaintext, and re-hydrated at load time. The default
    /// derived `Serialize` skips this field; the only place secrets are exposed
    /// on the wire is the auth-provider IPC boundary, which re-merges them into
    /// the flat `fields` map the external provider expects.
    ///
    /// A secret saved before this routing existed may still live in `fields` as
    /// plaintext until startup migration moves it here. That migration is
    /// deferred (never destructive) when the keyring is unavailable, so such a
    /// legacy value can remain in `fields` for a session.
    #[serde(skip)]
    pub secret_fields: HashMap<String, SecretString>,
    pub enabled: bool,
}

impl std::fmt::Debug for AuthProfile {
    /// Redacts secret values: `secret_fields` prints its key names only, never
    /// the `SecretString` contents. Mirrors the `ResolvedCredentials` Debug
    /// policy so `{:?}` in logs can never leak credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let secret_keys: Vec<&str> = self.secret_fields.keys().map(String::as_str).collect();

        f.debug_struct("AuthProfile")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("provider_id", &self.provider_id)
            .field("fields", &self.fields)
            .field("secret_fields", &secret_keys)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl<'de> Deserialize<'de> for AuthProfile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        let mut value = serde_json::Value::deserialize(deserializer)?;

        let obj = value
            .as_object_mut()
            .ok_or_else(|| D::Error::custom("AuthProfile must be a JSON object"))?;

        let id: Uuid = obj
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| D::Error::custom("missing field 'id'"))
            .and_then(|s| Uuid::parse_str(s).map_err(|e| D::Error::custom(e.to_string())))?;

        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| D::Error::custom("missing field 'name'"))?
            .to_string();

        let enabled = obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

        // Determine fields and provider_id, handling both new and old formats.
        let (fields, provider_id) = if let Some(fields_val) = obj.get("fields") {
            // New format: "fields" key is already a flat map.
            let fields: HashMap<String, String> =
                serde_json::from_value(fields_val.clone()).map_err(D::Error::custom)?;

            let provider_id = obj
                .get("provider_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            (fields, provider_id)
        } else if let Some(config_val) = obj.get("config") {
            // Old format: "config" is an object with a "type" discriminant.
            let config_obj = config_val
                .as_object()
                .ok_or_else(|| D::Error::custom("'config' must be a JSON object"))?;

            let old_provider_id = obj
                .get("provider_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let provider_id = old_provider_id.to_string();

            // Flatten config object into fields, dropping the "type" key.
            let mut fields = HashMap::new();
            for (key, val) in config_obj {
                if key == "type" {
                    continue;
                }
                let field_str = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                fields.insert(key.clone(), field_str);
            }

            (fields, provider_id)
        } else {
            // No fields and no config: empty fields, read provider_id as-is.
            let provider_id = obj
                .get("provider_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            (HashMap::new(), provider_id)
        };

        Ok(AuthProfile {
            id,
            name,
            provider_id,
            fields,
            secret_fields: HashMap::new(),
            enabled,
        })
    }
}

impl AuthProfile {
    pub fn new(
        name: impl Into<String>,
        provider_id: impl Into<String>,
        fields: HashMap<String, String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            provider_id: provider_id.into(),
            fields,
            secret_fields: HashMap::new(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthProfileSummary {
    pub id: Uuid,
    pub name: String,
    pub provider_id: String,
}

impl From<&AuthProfile> for AuthProfileSummary {
    fn from(profile: &AuthProfile) -> Self {
        Self {
            id: profile.id,
            name: profile.name.clone(),
            provider_id: profile.provider_id.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum AuthSessionState {
    Valid {
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    Expired,
    LoginRequired,
}

#[derive(Clone)]
pub struct AuthSession {
    pub provider_id: String,
    pub profile_id: Uuid,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,

    /// Provider-specific opaque session data.
    pub data: Option<Arc<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for AuthSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthSession")
            .field("provider_id", &self.provider_id)
            .field("profile_id", &self.profile_id)
            .field("expires_at", &self.expires_at)
            .field("data", &self.data.as_ref().map(|_| "<opaque>"))
            .finish()
    }
}

#[derive(Default)]
pub struct ResolvedCredentials {
    /// Plain-text credential fields (e.g. `"region"`, `"access_key_id"`).
    pub fields: HashMap<String, String>,

    /// Secret credential fields — values are redacted in logs.
    /// Keys use the same naming convention as `fields`
    /// (e.g. `"secret_access_key"`, `"session_token"`).
    pub secret_fields: HashMap<String, secrecy::SecretString>,

    /// Provider-specific opaque data carried with the resolved credentials.
    pub provider_data: Option<Arc<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for ResolvedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print secret_fields key names only — never expose the values.
        let secret_keys: Vec<&str> = self.secret_fields.keys().map(String::as_str).collect();

        f.debug_struct("ResolvedCredentials")
            .field("fields", &self.fields)
            .field("secret_fields", &secret_keys)
            .field(
                "provider_data",
                &self.provider_data.as_ref().map(|_| "<opaque>"),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_uuid() -> String {
        "00000000-0000-0000-0000-000000000001".to_string()
    }

    #[test]
    fn legacy_auth_config_keeps_provider_id() {
        let profile: AuthProfile = serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(), "name": "Legacy", "provider_id": "aws",
            "config": {"type": "aws_sso", "profile_name": "legacy"}
        }))
        .expect("legacy profile");
        assert_eq!(profile.provider_id, "aws");
        assert_eq!(
            profile.fields.get("profile_name").map(String::as_str),
            Some("legacy")
        );
    }

    #[test]
    fn new_format_roundtrip() {
        let mut fields = HashMap::new();
        fields.insert("profile_name".to_string(), "prod".to_string());
        fields.insert("region".to_string(), "us-west-2".to_string());

        let original = AuthProfile {
            id: Uuid::parse_str(&make_uuid()).unwrap(),
            name: "Prod".to_string(),
            provider_id: "aws-sso".to_string(),
            fields: fields.clone(),
            secret_fields: HashMap::new(),
            enabled: true,
        };

        let serialized = serde_json::to_value(&original).unwrap();
        let deserialized: AuthProfile = serde_json::from_value(serialized).unwrap();

        assert_eq!(deserialized.id, original.id);
        assert_eq!(deserialized.name, original.name);
        assert_eq!(deserialized.provider_id, original.provider_id);
        assert_eq!(deserialized.fields, original.fields);
        assert_eq!(deserialized.enabled, original.enabled);
    }

    #[test]
    fn serialize_omits_secret_fields() {
        let mut secret_fields = HashMap::new();
        secret_fields.insert(
            "secret_access_key".to_string(),
            SecretString::from("AKIAVERYSECRETVALUE".to_string()),
        );

        let mut fields = HashMap::new();
        fields.insert("region".to_string(), "us-east-1".to_string());

        let profile = AuthProfile {
            id: Uuid::parse_str(&make_uuid()).unwrap(),
            name: "Static".to_string(),
            provider_id: "custom".to_string(),
            fields,
            secret_fields,
            enabled: true,
        };

        let json = serde_json::to_string(&profile).unwrap();

        assert!(
            !json.contains("AKIAVERYSECRETVALUE"),
            "serialized profile must not contain the secret value: {json}"
        );
        assert!(
            !json.contains("secret_access_key"),
            "serialized profile must not even name the secret field: {json}"
        );
        assert!(
            json.contains("region"),
            "non-secret fields must still serialize: {json}"
        );
    }

    #[test]
    fn debug_redacts_secret_values() {
        let mut secret_fields = HashMap::new();
        secret_fields.insert(
            "secret_access_key".to_string(),
            SecretString::from("AKIAVERYSECRETVALUE".to_string()),
        );

        let profile = AuthProfile {
            id: Uuid::parse_str(&make_uuid()).unwrap(),
            name: "Static".to_string(),
            provider_id: "custom".to_string(),
            fields: HashMap::new(),
            secret_fields,
            enabled: true,
        };

        let debug = format!("{profile:?}");

        assert!(
            !debug.contains("AKIAVERYSECRETVALUE"),
            "Debug must never print the secret value: {debug}"
        );
        assert!(
            debug.contains("secret_access_key"),
            "Debug should still list the secret field key: {debug}"
        );
    }
}
