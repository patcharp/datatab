use super::*;

/// Records the profile each connect hands it.
struct RecordingDriver {
    inner: Arc<TestDriver>,
    connected: Mutex<Vec<ConnectionProfile>>,
}

impl DbDriver for RecordingDriver {
    fn kind(&self) -> DbKind {
        self.inner.kind()
    }

    fn metadata(&self) -> &DriverMetadata {
        self.inner.metadata()
    }

    fn form_definition(&self) -> &DriverFormDef {
        self.inner.form_definition()
    }

    fn driver_key(&self) -> crate::DriverKey {
        self.inner.driver_key()
    }

    fn build_config(&self, values: &FormValues) -> Result<DbConfig, DbError> {
        self.inner.build_config(values)
    }

    fn extract_values(&self, config: &DbConfig) -> FormValues {
        self.inner.extract_values(config)
    }

    fn connect_with_secrets(
        &self,
        profile: &ConnectionProfile,
        password: Option<&SecretString>,
        ssh_secret: Option<&SecretString>,
    ) -> Result<Box<dyn Connection>, DbError> {
        self.connected
            .lock()
            .map_err(|_| DbError::connection_failed("recording lock poisoned"))?
            .push(profile.clone());
        self.inner
            .connect_with_secrets(profile, password, ssh_secret)
    }

    fn test_connection(&self, profile: &ConnectionProfile) -> Result<(), DbError> {
        self.inner.test_connection(profile)
    }
}

#[derive(Default)]
struct MemoryStore {
    values: Mutex<HashMap<String, SecretString>>,
}

impl SecretStore for MemoryStore {
    fn is_available(&self) -> bool {
        true
    }

    #[expect(
        clippy::unwrap_in_result,
        reason = "test fixture: a poisoned lock means the test already panicked"
    )]
    fn get(&self, secret_ref: &str) -> Result<Option<SecretString>, DbError> {
        Ok(self
            .values
            .lock()
            .expect("store lock")
            .get(secret_ref)
            .cloned())
    }

    #[expect(
        clippy::unwrap_in_result,
        reason = "test fixture: a poisoned lock means the test already panicked"
    )]
    fn set(&self, secret_ref: &str, value: &SecretString) -> Result<(), DbError> {
        self.values
            .lock()
            .expect("store lock")
            .insert(secret_ref.to_string(), value.clone());
        Ok(())
    }

    fn delete(&self, _secret_ref: &str) -> Result<(), DbError> {
        Ok(())
    }
}

#[test]
fn connect_hands_the_driver_a_secret_field_kept_in_the_keyring() {
    let mut profile = ConnectionProfile::new_with_driver(
        "rpc",
        DbKind::SQLite,
        "rpc:fake",
        DbConfig::External {
            kind: DbKind::SQLite,
            values: HashMap::from([("host".to_string(), "rpc.example".to_string())]),
        },
    );
    profile
        .keyring_secret_fields
        .insert("api_token".to_string());
    let store = MemoryStore::default();
    store
        .set(
            &crate::connection_field_secret_ref(&profile.id, "api_token"),
            &SecretString::from("keyring-token"),
        )
        .expect("seed the store");
    let secret_store: Arc<RwLock<Box<dyn SecretStore>>> = Arc::new(RwLock::new(Box::new(store)));
    let driver = Arc::new(RecordingDriver {
        inner: TestDriver::postgres(),
        connected: Mutex::new(Vec::new()),
    });
    let manager = ConnectionManager::new(HashMap::from([(
        profile.driver_id(),
        driver.clone() as Arc<dyn DbDriver>,
    )]));

    let params = manager
        .prepare_connect_profile(
            profile.id,
            std::slice::from_ref(&profile),
            &[],
            &[],
            &secret_store,
            |_, _| None,
            None,
        )
        .expect("prepare");
    let result = params.execute(None).expect("connect");

    let connected = driver.connected.lock().expect("recording lock");
    let DbConfig::External { values, .. } = &connected[0].config else {
        panic!("not an external config");
    };
    assert_eq!(
        values.get("api_token").map(|value| value.as_str()),
        Some("keyring-token")
    );
    let DbConfig::External { values, .. } = &result.profile.config else {
        panic!("not an external config");
    };
    assert!(
        !values.contains_key("api_token"),
        "the connected profile the app keeps never holds the value"
    );
}

/// A marked secret field whose keyring entry is missing, or whose store has
/// no keyring behind it, refuses the connect before the driver is called
/// (#411 triage round 2, point 2).
#[test]
fn connect_refuses_a_marked_field_the_keyring_cannot_give() {
    let stores: [Box<dyn SecretStore>; 2] = [
        Box::new(MemoryStore::default()),
        Box::new(crate::NoopSecretStore),
    ];
    for store in stores {
        let mut profile = ConnectionProfile::new_with_driver(
            "rpc",
            DbKind::SQLite,
            "rpc:fake",
            DbConfig::External {
                kind: DbKind::SQLite,
                values: HashMap::from([("host".to_string(), "rpc.example".to_string())]),
            },
        );
        profile
            .keyring_secret_fields
            .insert("api_token".to_string());
        let secret_store: Arc<RwLock<Box<dyn SecretStore>>> = Arc::new(RwLock::new(store));
        let driver = Arc::new(RecordingDriver {
            inner: TestDriver::postgres(),
            connected: Mutex::new(Vec::new()),
        });
        let manager = ConnectionManager::new(HashMap::from([(
            profile.driver_id(),
            driver.clone() as Arc<dyn DbDriver>,
        )]));

        let params = manager
            .prepare_connect_profile(
                profile.id,
                std::slice::from_ref(&profile),
                &[],
                &[],
                &secret_store,
                |_, _| None,
                None,
            )
            .expect("prepare");
        let error = match params.execute(None) {
            Ok(_) => panic!("the connect must be refused"),
            Err(error) => error,
        };

        assert!(error.contains("api_token"), "{error}");
        assert!(
            driver.connected.lock().expect("recording lock").is_empty(),
            "the driver is never called"
        );
    }
}
