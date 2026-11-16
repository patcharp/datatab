use super::*;

#[tokio::test]
async fn mcp_refuses_before_pre_connect_hooks_on_direct_path() {
    assert_refused_before_hooks(false).await;
}

#[tokio::test]
async fn mcp_refuses_before_pre_connect_hooks_on_pipeline_path() {
    assert_refused_before_hooks(true).await;
}

async fn assert_refused_before_hooks(pipeline: bool) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let trace = temp_dir.path().join("forbidden-hook.log");
    let mut driver = RecordingDriver::new(FakeDriver::new(DbKind::Postgres));
    driver.refuses_profile = true;
    let mut hook = process_hook(&trace, "pre_connect", false);
    hook.env
        .insert("DATATAB_MCP_HOOK_TEST_PANIC".into(), "1".into());
    let mut profile = pipeline_profile_with_hooks(ConnectionHooks {
        pre_connect: vec![hook],
        ..Default::default()
    });
    if !pipeline {
        profile.access_kind = None;
        profile.value_refs.clear();
        profile.auth_profile_id = None;
    }
    assert_eq!(profile.uses_pipeline(), pipeline);
    let connection_id = profile.id.to_string();
    let state = test_state_with_driver("postgres", Arc::new(driver), profile);
    let error = match McpConnectionFactory::new(state)
        .connect(&connection_id, None)
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("driver refusal was ignored"),
    };
    assert!(error.contains("fixture refusal"), "{error}");
    assert!(!trace.exists(), "refused profile reached PreConnect hooks");
}
