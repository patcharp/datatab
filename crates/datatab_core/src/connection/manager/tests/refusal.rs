use super::*;

fn refusing_driver() -> Arc<TestDriver> {
    let mut driver = TestDriver::postgres();
    Arc::get_mut(&mut driver)
        .expect("sole driver owner")
        .refuses_profile = true;
    driver
}

#[test]
fn legacy_proxy_execute_refuses_before_tunnel() {
    let mut profile = ConnectionProfile::new("refused", DbConfig::default_postgres());
    let proxy = ProxyProfile::new("fixture", crate::ProxyKind::Socks5);
    profile.proxy_profile_id = Some(proxy.id);
    assert!(profile.access_kind.is_none());
    let params = ConnectProfileParams {
        profile,
        driver: refusing_driver(),
        secret_store: None,
        ssh_secret: None,
        proxy: Some(ResolvedProxy {
            profile: proxy,
            secret: None,
        }),
        password_override: None,
    };
    let result = params.execute(Some(|_, _, _| panic!("refused profile opened a proxy")));
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("driver refusal was ignored"),
    };
    assert!(error.contains("fixture refusal"), "{error}");
}

#[test]
fn legacy_prepare_refuses_before_secrets_and_pre_connect_hooks() {
    let profile = ConnectionProfile::new("refused", DbConfig::default_postgres());
    let manager = ConnectionManager::new(HashMap::from([(
        profile.driver_id(),
        refusing_driver() as Arc<dyn DbDriver>,
    )]));
    let secret_store: Arc<RwLock<Box<dyn SecretStore>>> =
        Arc::new(RwLock::new(Box::new(crate::NoopSecretStore)));
    let result = manager.prepare_connect_profile(
        profile.id,
        &[profile],
        &[],
        &[],
        &secret_store,
        |_, _| panic!("refused profile resolved SSH secrets"),
        None,
    );
    match result {
        Err(error) => assert!(error.to_string().contains("fixture refusal")),
        Ok(_) => panic!("refused profile reached PreConnect hooks"),
    }
}
