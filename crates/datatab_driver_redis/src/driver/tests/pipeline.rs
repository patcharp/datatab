use super::*;
use datatab_core::access::{AccessHandle, AccessKind, AccessManager};
use datatab_core::values::{CompositeValueResolver, ValueCache, ValueRef};
use datatab_core::{CancelToken, PipelineInput, pipeline_state_channel, run_pipeline};
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

// These fixtures complete synchronously and must never wait on external work.
fn run_ready<F: Future>(future: F) -> F::Output {
    struct NoopWake;
    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }
    let waker = Waker::from(Arc::new(NoopWake));
    match pin!(future).poll(&mut Context::from_waker(&waker)) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("pipeline fixture unexpectedly waited on external work"),
    }
}

struct ForbiddenAccess;

#[async_trait::async_trait]
impl AccessManager for ForbiddenAccess {
    async fn open(&self, _: &AccessKind, _: &str, _: u16) -> Result<AccessHandle, DbError> {
        panic!("refused Sentinel profile reached the access manager");
    }
}

fn input(profile: ConnectionProfile, access_manager: Arc<dyn AccessManager>) -> PipelineInput {
    PipelineInput {
        profile,
        driver: Arc::new(RedisDriver::new()),
        auth_provider: None,
        auth_profile: None,
        resolver: CompositeValueResolver::new(Arc::new(ValueCache::new(Duration::ZERO))),
        access_manager,
        cancel: CancelToken::new(),
    }
}

fn assert_refused_before_access(kind: AccessKind) {
    let profiles = [
        sentinel_profile("localhost", None, None, None, true),
        sentinel_profile("127.0.0.1", None, Some("on"), None, false),
        sentinel_profile("::1", None, Some("verify"), None, false),
        sentinel_profile(
            "127.0.0.1",
            Some("rediss://127.0.0.1:1/"),
            None,
            None,
            false,
        ),
        sentinel_profile(
            "127.0.0.1",
            Some("REDISS://127.0.0.1:1/#insecure "),
            None,
            None,
            false,
        ),
    ];
    for mut profile in profiles {
        profile.access_kind = Some(kind.clone());
        let (sender, _watcher) = pipeline_state_channel();
        let error = match run_ready(run_pipeline(
            input(profile, Arc::new(ForbiddenAccess)),
            &sender,
        )) {
            Err(error) => error,
            Ok(_) => panic!("TLS Sentinel should be refused"),
        };
        assert_eq!(error.stage, "driver_connect");
        assert!(matches!(error.source, DbError::InvalidProfile(message)
            if message.contains("cannot reach the Sentinel master over TLS yet")));
    }
}

#[test]
fn sentinel_pipeline_refuses_direct_before_access() {
    assert_refused_before_access(AccessKind::Direct);
}

#[test]
fn sentinel_pipeline_refuses_ssh_before_access() {
    assert_refused_before_access(AccessKind::Ssh {
        ssh_tunnel_profile_id: uuid::Uuid::new_v4(),
    });
}

#[test]
fn sentinel_pipeline_refuses_proxy_before_access() {
    assert_refused_before_access(AccessKind::Proxy {
        proxy_profile_id: uuid::Uuid::new_v4(),
    });
}

#[test]
fn sentinel_pipeline_refuses_before_secret_resolution() {
    let mut profile = sentinel_profile("127.0.0.1", None, Some("on"), None, false);
    profile.value_refs.insert(
        "password".into(),
        ValueRef::secret("unavailable", "unused", None),
    );
    let (sender, _watcher) = pipeline_state_channel();
    let error = match run_ready(run_pipeline(
        input(profile, Arc::new(ForbiddenAccess)),
        &sender,
    )) {
        Err(error) => error,
        Ok(_) => panic!("TLS Sentinel should be refused"),
    };
    assert_eq!(
        error.stage, "driver_connect",
        "refusal must precede secret resolution"
    );
}

#[test]
fn sentinel_validation_does_not_read_certificates() {
    let mut profile = sentinel_profile("::1", None, Some("verify"), None, false);
    if let DbConfig::Redis {
        ssl_root_cert_path, ..
    } = &mut profile.config
    {
        *ssl_root_cert_path = Some("/nonexistent/datatab-test-ca.pem".into());
    }
    assert!(matches!(RedisDriver::new().validate_profile(&profile),
        Err(DbError::InvalidProfile(message))
            if message.contains("cannot reach the Sentinel master over TLS yet")));
}

#[test]
fn sentinel_validation_refuses_unknown_mode() {
    let profile = sentinel_profile("localhost", None, Some("unknown"), None, false);
    assert!(matches!(RedisDriver::new().validate_profile(&profile),
        Err(DbError::InvalidProfile(message)) if message == "Unknown Redis SSL mode: 'unknown'"));
}

#[test]
fn sentinel_pipeline_refuses_unknown_mode_before_access() {
    let profile = sentinel_profile("localhost", None, Some("unknown"), None, false);
    let (sender, _watcher) = pipeline_state_channel();
    let error = match run_ready(run_pipeline(
        input(profile, Arc::new(ForbiddenAccess)),
        &sender,
    )) {
        Err(error) => error,
        Ok(_) => panic!("unknown Sentinel TLS mode was accepted"),
    };
    assert!(
        matches!(error.source, DbError::InvalidProfile(message) if message == "Unknown Redis SSL mode: 'unknown'")
    );
}

#[test]
fn sentinel_validation_refuses_legacy_tls_true_without_ssl_mode() {
    let profile = sentinel_profile("localhost", None, None, None, true);
    assert!(matches!(RedisDriver::new().validate_profile(&profile),
        Err(DbError::InvalidProfile(message))
            if message.contains("cannot reach the Sentinel master over TLS yet")));
}

struct ForbiddenAuth;

impl datatab_core::auth::AuthProvider for ForbiddenAuth {
    fn provider_id(&self) -> &'static str {
        "forbidden"
    }
    fn display_name(&self) -> &'static str {
        "Forbidden auth"
    }
    async fn validate_session(
        &self,
        _: &datatab_core::AuthProfile,
    ) -> Result<datatab_core::AuthSessionState, DbError> {
        panic!("refused profile reached authentication");
    }
    async fn login(
        &self,
        _: &datatab_core::AuthProfile,
    ) -> Result<datatab_core::AuthSession, DbError> {
        panic!("refused profile reached login");
    }
    async fn resolve_credentials(
        &self,
        _: &datatab_core::AuthProfile,
    ) -> Result<datatab_core::ResolvedCredentials, DbError> {
        panic!("refused profile resolved credentials");
    }
}

#[test]
fn sentinel_pipeline_refuses_before_authentication() {
    let profile = sentinel_profile("localhost", None, Some("on"), None, false);
    let mut input = input(profile, Arc::new(ForbiddenAccess));
    input.auth_provider = Some(Box::new(ForbiddenAuth));
    input.auth_profile = Some(datatab_core::AuthProfile::new(
        "fixture",
        "forbidden",
        HashMap::new(),
    ));
    let (sender, _watcher) = pipeline_state_channel();
    let error = match run_ready(run_pipeline(input, &sender)) {
        Err(error) => error,
        Ok(_) => panic!("driver refusal was ignored"),
    };
    assert_eq!(error.stage, "driver_connect");
}

struct ExpectedAccess(AccessKind);

#[async_trait::async_trait]
impl AccessManager for ExpectedAccess {
    async fn open(
        &self,
        kind: &AccessKind,
        host: &str,
        port: u16,
    ) -> Result<AccessHandle, DbError> {
        assert_eq!(
            serde_json::to_value(kind).expect("kind"),
            serde_json::to_value(&self.0).expect("kind")
        );
        assert_eq!((host, port), ("localhost", 6380));
        Ok(if matches!(kind, AccessKind::Direct) {
            AccessHandle::direct()
        } else {
            AccessHandle::tunnel(16380, Box::new(()))
        })
    }
}

#[test]
fn valid_profiles_keep_direct_ssh_and_proxy_access_and_resolved_values() {
    for topology in ["standalone", "cluster", "sentinel"] {
        for kind in [
            AccessKind::Direct,
            AccessKind::Ssh {
                ssh_tunnel_profile_id: uuid::Uuid::new_v4(),
            },
            AccessKind::Proxy {
                proxy_profile_id: uuid::Uuid::new_v4(),
            },
        ] {
            let mut profile = sentinel_profile("unresolved", None, Some("off"), None, false);
            if let DbConfig::Redis {
                topology: configured,
                ..
            } = &mut profile.config
            {
                *configured = Some(topology.into());
            }
            profile.access_kind = Some(kind.clone());
            profile.value_refs.extend([
                ("host".into(), ValueRef::literal("localhost")),
                ("port".into(), ValueRef::literal("6380")),
                ("password".into(), ValueRef::literal("fixture-password")),
            ]);
            let (sender, _watcher) = pipeline_state_channel();
            let output = run_ready(run_pipeline(
                input(profile, Arc::new(ExpectedAccess(kind.clone()))),
                &sender,
            ))
            .expect("valid profile must reach access setup");
            let tunneled = !matches!(kind, AccessKind::Direct);
            assert_eq!(output.access_handle.is_tunneled(), tunneled);
            assert_eq!(
                output.access_handle.local_port(),
                if tunneled { 16380 } else { 0 }
            );
            assert_eq!(
                output.resolved_profile.config.host_port(),
                Some(("localhost", 6380))
            );
            assert_eq!(
                output.resolved_password.expect("password").expose_secret(),
                "fixture-password"
            );
        }
    }
}

#[test]
fn standalone_and_cluster_tls_remain_accepted_through_pipeline() {
    for topology in ["standalone", "cluster"] {
        for (ssl_mode, uri) in [
            (Some("on"), None),
            (Some("verify"), None),
            (None, Some("rediss://localhost:6380/")),
        ] {
            let mut profile = sentinel_profile("localhost", uri, ssl_mode, None, false);
            if let DbConfig::Redis {
                topology: configured,
                port,
                ssl_root_cert_path,
                ..
            } = &mut profile.config
            {
                *configured = Some(topology.into());
                *port = 6380;
                *ssl_root_cert_path = Some("/nonexistent/datatab-test-ca.pem".into());
            }
            let (sender, _watcher) = pipeline_state_channel();
            run_ready(run_pipeline(
                input(profile, Arc::new(ExpectedAccess(AccessKind::Direct))),
                &sender,
            ))
            .expect("supported TLS topology must pass the pipeline without reading certificates");
        }
    }
}
