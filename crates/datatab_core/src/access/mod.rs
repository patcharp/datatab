use crate::DbError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// How the app connects to the remote database host.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum AccessKind {
    #[default]
    Direct,
    Ssh {
        ssh_tunnel_profile_id: Uuid,
    },
    Proxy {
        proxy_profile_id: Uuid,
    },
    /// Generic managed-access variant. `provider` identifies the access
    /// backend. `params` carries the keys that backend defines.
    Managed {
        provider: String,
        #[serde(default)]
        params: HashMap<String, String>,
    },
}

pub struct AccessHandle {
    local_port: u16,
    _handle: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl AccessHandle {
    pub fn direct() -> Self {
        Self {
            local_port: 0,
            _handle: None,
        }
    }

    pub fn tunnel(local_port: u16, handle: Box<dyn std::any::Any + Send + Sync>) -> Self {
        Self {
            local_port,
            _handle: Some(handle),
        }
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub fn is_tunneled(&self) -> bool {
        self._handle.is_some()
    }
}

/// Abstraction over tunnel/access setup (SSH, proxy, managed, direct).
///
/// The app crate provides the concrete implementation that dispatches
/// to the right tunnel infrastructure based on the `AccessKind` variant.
#[async_trait::async_trait]
pub trait AccessManager: Send + Sync {
    async fn open(
        &self,
        access_kind: &AccessKind,
        remote_host: &str,
        remote_port: u16,
    ) -> Result<AccessHandle, DbError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_roundtrip() {
        let mut params = HashMap::new();
        params.insert("instance_id".to_string(), "i-test".to_string());
        params.insert("region".to_string(), "ap-east-1".to_string());

        let original = AccessKind::Managed {
            provider: "aws-ssm".to_string(),
            params: params.clone(),
        };

        let serialized = serde_json::to_value(&original).unwrap();
        let deserialized: AccessKind = serde_json::from_value(serialized).unwrap();

        match deserialized {
            AccessKind::Managed {
                provider,
                params: p,
            } => {
                assert_eq!(provider, "aws-ssm");
                assert_eq!(p, params);
            }
            other => panic!("expected Managed, got {:?}", other),
        }
    }

    /// The JSON written for each kept variant, which driver hosts read from
    /// `OpenSession.profile_json`. Both directions are pinned to the literal
    /// text so the shape cannot drift.
    #[test]
    fn kept_variants_keep_their_json_shape() {
        let tunnel_id = "00000000-0000-0000-0000-0000000000a1";
        let proxy_id = "00000000-0000-0000-0000-0000000000b2";
        let cases = [
            (AccessKind::Direct, r#"{"method":"direct"}"#.to_string()),
            (
                AccessKind::Ssh {
                    ssh_tunnel_profile_id: tunnel_id.parse().unwrap(),
                },
                format!(r#"{{"method":"ssh","ssh_tunnel_profile_id":"{tunnel_id}"}}"#),
            ),
            (
                AccessKind::Proxy {
                    proxy_profile_id: proxy_id.parse().unwrap(),
                },
                format!(r#"{{"method":"proxy","proxy_profile_id":"{proxy_id}"}}"#),
            ),
            (
                AccessKind::Managed {
                    provider: "custom-provider".to_string(),
                    params: HashMap::from([("target".to_string(), "db-1".to_string())]),
                },
                r#"{"method":"managed","provider":"custom-provider","params":{"target":"db-1"}}"#
                    .to_string(),
            ),
            (
                AccessKind::Managed {
                    provider: "custom-provider".to_string(),
                    params: HashMap::new(),
                },
                r#"{"method":"managed","provider":"custom-provider","params":{}}"#.to_string(),
            ),
        ];

        for (kind, json) in cases {
            assert_eq!(serde_json::to_string(&kind).unwrap(), json);

            let parsed: AccessKind = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
        }
    }

    #[test]
    fn managed_without_params_key_reads_as_empty_params() {
        let parsed: AccessKind =
            serde_json::from_str(r#"{"method":"managed","provider":"custom-provider"}"#).unwrap();

        match parsed {
            AccessKind::Managed { provider, params } => {
                assert_eq!(provider, "custom-provider");
                assert!(params.is_empty());
            }
            other => panic!("expected Managed, got {:?}", other),
        }
    }

    #[test]
    fn legacy_ssm_method_is_rejected() {
        let json =
            r#"{"method":"ssm","instance_id":"i-abc","region":"us-east-1","remote_port":5432}"#;

        let error = serde_json::from_str::<AccessKind>(json)
            .expect_err("the removed legacy form must not parse");

        assert!(
            error.to_string().contains("unknown variant `ssm`"),
            "{error}"
        );
    }

    #[test]
    fn direct_ssh_proxy_unaffected() {
        let cases = vec![
            serde_json::json!({"method": "direct"}),
            serde_json::json!({"method": "ssh", "ssh_tunnel_profile_id": Uuid::new_v4().to_string()}),
            serde_json::json!({"method": "proxy", "proxy_profile_id": Uuid::new_v4().to_string()}),
        ];

        for case in cases {
            let method = case["method"].as_str().unwrap().to_string();
            let kind: AccessKind = serde_json::from_value(case).unwrap();
            match (&kind, method.as_str()) {
                (AccessKind::Direct, "direct") => {}
                (AccessKind::Ssh { .. }, "ssh") => {}
                (AccessKind::Proxy { .. }, "proxy") => {}
                _ => panic!("unexpected variant for method '{}'", method),
            }
        }
    }
}
