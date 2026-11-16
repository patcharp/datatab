use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use datatab_core::secrecy::{ExposeSecret, SecretString};
use datatab_core::values::{
    CompositeValueResolver, DynSecretProvider, ResolveContext, ValueCache, ValueRef,
};
use datatab_core::{ConnectionProfile, DbConfig, DbError, resolve_profile_values};

use super::SecretProviderRegistry;

struct NamedSecretProvider {
    provider_id: &'static str,
    display_name: &'static str,
}

#[async_trait::async_trait]
impl DynSecretProvider for NamedSecretProvider {
    fn provider_id(&self) -> &str {
        self.provider_id
    }

    fn display_name(&self) -> &str {
        self.display_name
    }

    async fn get_secret(
        &self,
        _locator: &str,
        _json_key: Option<&str>,
    ) -> Result<SecretString, DbError> {
        Ok(SecretString::from("unused".to_string()))
    }
}

fn registry_with(entries: &[(&'static str, &'static str)]) -> SecretProviderRegistry {
    let mut registry = SecretProviderRegistry::new();
    for (provider_id, display_name) in entries {
        registry.register(Arc::new(NamedSecretProvider {
            provider_id,
            display_name,
        }));
    }
    registry
}

#[test]
fn list_is_sorted_by_display_name() {
    let registry = registry_with(&[("zeta", "Alpha Vault"), ("alpha", "Zeta Vault")]);

    assert_eq!(
        registry.list(),
        vec![
            ("zeta".to_string(), "Alpha Vault".to_string()),
            ("alpha".to_string(), "Zeta Vault".to_string()),
        ]
    );
}

#[test]
fn contains_reports_registered_ids() {
    let registry = registry_with(&[("vault", "Vault")]);

    assert!(registry.contains("vault"));
    assert!(!registry.contains("aws-ssm"));
}

#[test]
fn register_into_makes_providers_resolvable() {
    let registry = registry_with(&[("vault", "Vault"), ("keeper", "Keeper")]);
    let mut resolver =
        CompositeValueResolver::new(Arc::new(ValueCache::new(Duration::from_secs(60))));

    registry.register_into(&mut resolver);

    assert!(resolver.has_secret_provider("vault"));
    assert!(resolver.has_secret_provider("keeper"));
    assert!(!resolver.has_secret_provider("aws-secrets-manager"));
}

/// Serves `secret-for-<locator>` and counts every request it gets.
struct CountingSecretProvider {
    requests: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl DynSecretProvider for CountingSecretProvider {
    fn provider_id(&self) -> &str {
        "vault"
    }

    fn display_name(&self) -> &str {
        "Vault"
    }

    async fn get_secret(
        &self,
        locator: &str,
        _json_key: Option<&str>,
    ) -> Result<SecretString, DbError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        Ok(SecretString::from(format!("secret-for-{locator}")))
    }
}

fn profile_with_password_secret(provider: &str) -> ConnectionProfile {
    let mut profile = ConnectionProfile::new("secret source", DbConfig::default_postgres());
    profile.value_refs = HashMap::from([(
        "password".to_string(),
        ValueRef::secret(provider, "kv/db", None),
    )]);
    profile
}

/// A registered RPC secret provider still resolves a `ValueRef::Secret`. A
/// reference to a provider that is not registered, such as a former built-in
/// AWS id, fails the resolve stage naming the field: no provider is asked and
/// no value comes back for the field.
#[tokio::test]
async fn registered_provider_resolves_and_unregistered_fails_with_field() {
    let requests = Arc::new(AtomicUsize::new(0));
    let mut registry = SecretProviderRegistry::new();
    registry.register(Arc::new(CountingSecretProvider {
        requests: Arc::clone(&requests),
    }));
    let mut resolver =
        CompositeValueResolver::new(Arc::new(ValueCache::new(Duration::from_secs(60))));
    registry.register_into(&mut resolver);
    let context = ResolveContext::default();

    let (_, password) =
        resolve_profile_values(&profile_with_password_secret("vault"), &resolver, &context)
            .await
            .expect("a registered provider resolves");
    assert_eq!(
        password.expect("resolved password").expose_secret(),
        "secret-for-kv/db"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    for provider in ["aws-secrets-manager", "aws-ssm"] {
        let error = match resolve_profile_values(
            &profile_with_password_secret(provider),
            &resolver,
            &context,
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("an unregistered provider must not resolve"),
        };
        let DbError::ValueResolutionFailed(message) = error.source else {
            panic!(
                "expected a value resolution failure, got {:?}",
                error.source
            );
        };
        assert_eq!(
            message,
            format!(
                "Failed to resolve field 'password': Value resolution failed: \
                 Secret provider '{provider}' not registered"
            )
        );
    }
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "no provider was asked on behalf of the missing one"
    );
}
