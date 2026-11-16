use super::*;

struct ForbiddenAccess;

#[async_trait::async_trait]
impl AccessManager for ForbiddenAccess {
    async fn open(&self, _: &AccessKind, _: &str, _: u16) -> Result<AccessHandle, DbError> {
        panic!("refused profile reached access setup");
    }
}

#[tokio::test]
async fn pipeline_refuses_before_access() {
    for kind in [
        AccessKind::Direct,
        AccessKind::Ssh {
            ssh_tunnel_profile_id: uuid::Uuid::new_v4(),
        },
        AccessKind::Proxy {
            proxy_profile_id: uuid::Uuid::new_v4(),
        },
    ] {
        let mut profile = ConnectionProfile::new("refused", DbConfig::default_postgres());
        profile.access_kind = Some(kind);
        let input = PipelineInput {
            profile,
            driver: Arc::new(TestDriver {
                refuses_profile: true,
            }),
            auth_provider: None,
            auth_profile: None,
            resolver: test_resolver(),
            access_manager: Arc::new(ForbiddenAccess),
            cancel: CancelToken::new(),
        };
        let (sender, _watcher) = pipeline_state_channel();
        let error = match run_pipeline(input, &sender).await {
            Err(error) => error,
            Ok(_) => panic!("driver refusal was ignored"),
        };
        assert_eq!(error.stage, "driver_connect");
        assert!(
            matches!(error.source, DbError::InvalidProfile(message) if message == "fixture refusal")
        );
    }
}
