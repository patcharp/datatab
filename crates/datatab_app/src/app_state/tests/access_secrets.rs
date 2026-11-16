use super::refusal::PanicOnReadSecretStore;
use super::*;
use std::sync::Mutex;

struct CountingSecretStore {
    secrets: HashMap<String, SecretString>,
    reads: Arc<Mutex<Vec<String>>>,
}

impl SecretStore for CountingSecretStore {
    fn is_available(&self) -> bool {
        true
    }

    fn get(&self, reference: &str) -> Result<Option<SecretString>, DbError> {
        self.reads
            .lock()
            .map_err(|_| DbError::connection_failed("fixture lock poisoned"))?
            .push(reference.into());
        Ok(self.secrets.get(reference).cloned())
    }

    fn set(&self, reference: &str, secret: &SecretString) -> Result<(), DbError> {
        PanicOnReadSecretStore.set(reference, secret)
    }

    fn delete(&self, reference: &str) -> Result<(), DbError> {
        PanicOnReadSecretStore.delete(reference)
    }
}

fn access_state() -> (AppState, ConnectionProfile) {
    let profile = ConnectionProfile::new("fixture", DbConfig::default_postgres());
    let mut state = test_state_with_profiles(
        test_drivers(&profile.driver_id(), false, false),
        vec![profile.clone()],
    );
    for name in ["unrelated", "selected"] {
        state.facade.ssh_tunnels.items.push(SshTunnelProfile::new(
            name,
            datatab_core::SshTunnelConfig {
                host: "127.0.0.1".into(),
                port: 22,
                user: "fixture".into(),
                auth_method: datatab_core::SshAuthMethod::Password,
                ssh_config_host: None,
            },
        ));
        let mut proxy = datatab_core::ProxyProfile::new(name, datatab_core::ProxyKind::Socks5);
        proxy.save_secret = true;
        state.facade.proxies.items.push(proxy);
    }
    (state, profile)
}

fn count_access_reads(state: &mut AppState) -> Arc<Mutex<Vec<String>>> {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let references = state
        .facade
        .ssh_tunnels
        .items
        .iter()
        .map(SshTunnelProfile::secret_ref)
        .chain(
            state
                .facade
                .proxies
                .items
                .iter()
                .map(|proxy| proxy.secret_ref()),
        );
    let secrets = references
        .map(|reference| (reference, SecretString::from("fixture secret")))
        .collect();
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(CountingSecretStore {
        secrets,
        reads: Arc::clone(&reads),
    }));
    reads
}

#[test]
fn pipeline_reads_only_selected_tunnel_and_proxy_secrets() {
    for access_kind in [None, Some("ssh"), Some("proxy")] {
        let (mut state, mut profile) = access_state();
        let tunnel = &state.facade.ssh_tunnels.items[1];
        let proxy = &state.facade.proxies.items[1];
        profile.config.assign_ssh_tunnel(None, Some(tunnel.id));
        profile.proxy_profile_id = Some(proxy.id);
        let expected = match access_kind {
            Some("ssh") => {
                let tunnel = &state.facade.ssh_tunnels.items[0];
                profile.access_kind = Some(AccessKind::Ssh {
                    ssh_tunnel_profile_id: tunnel.id,
                });
                vec![tunnel.secret_ref()]
            }
            Some("proxy") => {
                let proxy = &state.facade.proxies.items[0];
                profile.access_kind = Some(AccessKind::Proxy {
                    proxy_profile_id: proxy.id,
                });
                vec![proxy.secret_ref()]
            }
            _ => Vec::new(),
        };
        let reads = count_access_reads(&mut state);
        state
            .build_pipeline_input_for_profile(profile, CancelToken::new())
            .expect("pipeline preparation");
        assert_eq!(*reads.lock().expect("reads lock"), expected);
    }
}

#[test]
fn pipeline_without_selected_access_profiles_reads_no_secrets() {
    for access_kind in [None, Some(AccessKind::Direct)] {
        let (mut state, mut profile) = access_state();
        profile.access_kind = access_kind;
        state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));
        state
            .build_pipeline_input_for_profile(profile, CancelToken::new())
            .expect("pipeline preparation");
    }
}

#[test]
fn pipeline_direct_with_legacy_access_ids_reads_no_secrets() {
    let (mut state, mut profile) = access_state();
    profile.access_kind = Some(AccessKind::Direct);
    profile
        .config
        .assign_ssh_tunnel(None, Some(state.facade.ssh_tunnels.items[0].id));
    profile.proxy_profile_id = Some(state.facade.proxies.items[0].id);
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));

    state
        .build_pipeline_input_for_profile(profile, CancelToken::new())
        .expect("pipeline preparation");
}

#[test]
fn pipeline_managed_with_legacy_access_ids_reads_no_secrets() {
    let (mut state, mut profile) = access_state();
    let auth_profile = AuthProfile::new("fixture", "fixture-auth", HashMap::new());
    profile.auth_profile_id = Some(auth_profile.id);
    state.facade.auth_profiles.items.push(auth_profile);
    state
        .auth_provider_registry
        .register(Arc::new(TestAuthProvider::new("fixture-auth")));
    profile.access_kind = Some(AccessKind::Managed {
        provider: "fixture-access".into(),
        params: HashMap::new(),
    });
    profile
        .config
        .assign_ssh_tunnel(None, Some(state.facade.ssh_tunnels.items[0].id));
    profile.proxy_profile_id = Some(state.facade.proxies.items[0].id);
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));

    state
        .build_pipeline_input_for_profile(profile, CancelToken::new())
        .expect("pipeline preparation");
}

#[test]
fn legacy_connect_still_reads_the_profiles_inline_ssh_secret() {
    let (mut state, mut profile) = access_state();
    profile.save_password = false;
    profile
        .config
        .assign_ssh_tunnel(Some(state.facade.ssh_tunnels.items[0].config.clone()), None);
    let reference = profile.ssh_secret_ref();
    let reads = Arc::new(Mutex::new(Vec::new()));
    state.facade.secrets = datatab_core::SecretManager::new(Box::new(CountingSecretStore {
        secrets: HashMap::from([(reference.clone(), SecretString::from("inline fixture"))]),
        reads: Arc::clone(&reads),
    }));
    state.facade.profiles.profiles = vec![profile.clone()];

    let prepared = state
        .prepare_connect_profile(profile.id)
        .expect("legacy connect preparation");
    assert!(prepared.ssh_secret.is_some());
    assert_eq!(*reads.lock().expect("reads lock"), vec![reference]);
}

#[tokio::test]
async fn pipeline_missing_selected_access_profile_keeps_the_error_without_secret_reads() {
    for (access_kind, label) in [
        (
            AccessKind::Ssh {
                ssh_tunnel_profile_id: Uuid::new_v4(),
            },
            "SSH tunnel profile",
        ),
        (
            AccessKind::Proxy {
                proxy_profile_id: Uuid::new_v4(),
            },
            "Proxy profile",
        ),
    ] {
        let (mut state, mut profile) = access_state();
        profile.access_kind = Some(access_kind.clone());
        state.facade.secrets = datatab_core::SecretManager::new(Box::new(PanicOnReadSecretStore));
        let input = state
            .build_pipeline_input_for_profile(profile, CancelToken::new())
            .expect("pipeline preparation");
        let error = match input
            .access_manager
            .open(&access_kind, "127.0.0.1", 5432)
            .await
        {
            Err(DbError::ConnectionFailed(error)) => error,
            _ => panic!("missing access profile must fail"),
        };
        let identifier = match access_kind {
            AccessKind::Ssh {
                ssh_tunnel_profile_id,
            } => ssh_tunnel_profile_id,
            AccessKind::Proxy { proxy_profile_id } => proxy_profile_id,
            _ => unreachable!(),
        };
        assert_eq!(
            error.message,
            format!("{label} '{identifier}' was not found")
        );
    }
}
