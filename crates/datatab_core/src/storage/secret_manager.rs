use crate::{ConnectionProfile, DbConfig, ProxyProfile, SecretStore, SshTunnelProfile};
use log::error;
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use std::sync::RwLock;

/// Unifies types that have a keyring secret reference (`secret_ref()`).
pub trait HasSecretRef {
    fn secret_ref(&self) -> String;
}

impl HasSecretRef for SshTunnelProfile {
    fn secret_ref(&self) -> String {
        self.secret_ref()
    }
}

impl HasSecretRef for ProxyProfile {
    fn secret_ref(&self) -> String {
        self.secret_ref()
    }
}

fn read_secret_store(
    secret_store: &RwLock<Box<dyn SecretStore>>,
) -> std::sync::RwLockReadGuard<'_, Box<dyn SecretStore>> {
    match secret_store.read() {
        Ok(guard) => guard,
        Err(poison_err) => {
            log::warn!("Secret store RwLock poisoned, recovering...");
            poison_err.into_inner()
        }
    }
}

/// Password and SSH entry operations over a shared store, without application state.
pub struct ProfileSecrets<'a> {
    secret_store: &'a RwLock<Box<dyn SecretStore>>,
}

impl<'a> ProfileSecrets<'a> {
    pub fn new(secret_store: &'a RwLock<Box<dyn SecretStore>>) -> Self {
        Self { secret_store }
    }

    fn store_read(&self) -> std::sync::RwLockReadGuard<'_, Box<dyn SecretStore>> {
        read_secret_store(self.secret_store)
    }

    pub fn save_password(
        &self,
        profile: &ConnectionProfile,
        password: &SecretString,
    ) -> Result<(), crate::DbError> {
        if !profile.save_password {
            return Ok(());
        }

        let store = self.store_read();

        if !store.is_available() {
            return Err(crate::DbError::NotSupported(
                "System keyring unavailable".to_string(),
            ));
        }

        store.set(&profile.secret_ref(), password)
    }

    pub fn delete_password(&self, profile: &ConnectionProfile) {
        let store = self.store_read();

        if !store.is_available() {
            return;
        }

        if let Err(e) = store.delete(&profile.secret_ref()) {
            error!("Failed to delete password: {:?}", e);
        }
    }

    pub fn save_ssh_password(&self, profile: &ConnectionProfile, secret: &SecretString) {
        let store = self.store_read();

        if !store.is_available() {
            log::warn!("Secret store unavailable; SSH password was NOT persisted");
            return;
        }

        if let Err(e) = store.set(&profile.ssh_secret_ref(), secret) {
            error!("Failed to save SSH secret: {:?}", e);
        }
    }

    pub fn delete_ssh_password(&self, profile: &ConnectionProfile) {
        let store = self.store_read();

        if !store.is_available() {
            return;
        }

        if let Err(e) = store.delete(&profile.ssh_secret_ref()) {
            error!("Failed to delete SSH secret: {:?}", e);
        }
    }
}

#[derive(Clone)]
pub struct SecretManager {
    secret_store: Arc<RwLock<Box<dyn SecretStore>>>,
}

impl SecretManager {
    pub fn new(secret_store: Box<dyn SecretStore>) -> Self {
        Self {
            secret_store: Arc::new(RwLock::new(secret_store)),
        }
    }

    fn store_read(&self) -> std::sync::RwLockReadGuard<'_, Box<dyn SecretStore>> {
        read_secret_store(&self.secret_store)
    }

    pub fn is_available(&self) -> bool {
        self.store_read().is_available()
    }

    pub fn secret_store_arc(&self) -> Arc<RwLock<Box<dyn SecretStore>>> {
        self.secret_store.clone()
    }

    /// Reads a secret stored under an explicit keyring reference.
    ///
    /// Used for per-field auth-profile secrets, whose reference is computed as
    /// `datatab:auth:{profile_id}:{field_id}` rather than derived from a single
    /// `HasSecretRef` value (a profile can hold several independent secrets).
    pub fn get_by_ref(&self, secret_ref: &str) -> Option<SecretString> {
        let store = self.store_read();

        if !store.is_available() {
            return None;
        }

        match store.get(secret_ref) {
            Ok(secret) => secret,
            Err(e) => {
                error!("Failed to get secret '{}': {:?}", secret_ref, e);
                None
            }
        }
    }

    /// Writes a secret under an explicit keyring reference.
    ///
    /// Returns `true` only when the value was actually persisted. A `false`
    /// result (store unavailable, or the backend rejected the write — e.g. a
    /// locked keyring) is logged, and callers that must not lose data (the
    /// plaintext->keyring migration) or that need to tell the user (the auth
    /// profile editor) MUST act on it rather than assume success.
    #[must_use]
    pub fn set_by_ref(&self, secret_ref: &str, secret: &SecretString) -> bool {
        let store = self.store_read();

        if !store.is_available() {
            log::warn!("Secret store unavailable; secret '{secret_ref}' was NOT persisted");
            return false;
        }

        match store.set(secret_ref, secret) {
            Ok(()) => true,
            Err(e) => {
                error!("Failed to save secret '{}': {:?}", secret_ref, e);
                false
            }
        }
    }

    /// Deletes an entry for a caller that must report a failed cleanup.
    /// Unlike best-effort deletion, an unavailable store is a failure too.
    pub fn try_delete_by_ref(&self, secret_ref: &str) -> Result<(), crate::DbError> {
        let store = self.store_read();
        if !store.is_available() {
            return Err(crate::DbError::NotSupported(
                "System keyring unavailable".into(),
            ));
        }
        store.delete(secret_ref)
    }

    /// Deletes a secret stored under an explicit keyring reference.
    pub fn delete_by_ref(&self, secret_ref: &str) {
        let store = self.store_read();

        if !store.is_available() {
            return;
        }

        if let Err(e) = store.delete(secret_ref) {
            log::warn!("Failed to delete secret '{}': {:?}", secret_ref, e);
        }
    }

    pub fn get_secret<T: HasSecretRef>(&self, item: &T, label: &str) -> Option<SecretString> {
        match self.store_read().get(&item.secret_ref()) {
            Ok(secret) => secret,
            Err(e) => {
                error!("Failed to get {} secret: {:?}", label, e);
                None
            }
        }
    }

    pub fn save_secret<T: HasSecretRef>(&self, item: &T, secret: &SecretString, label: &str) {
        let store = self.store_read();

        if !store.is_available() {
            log::warn!("Secret store unavailable; {label} secret was NOT persisted");
            return;
        }

        if let Err(e) = store.set(&item.secret_ref(), secret) {
            error!("Failed to save {} secret: {:?}", label, e);
        }
    }

    pub fn delete_secret<T: HasSecretRef>(&self, item: &T, label: &str) {
        let store = self.store_read();

        if !store.is_available() {
            return;
        }

        if let Err(e) = store.delete(&item.secret_ref()) {
            log::warn!("Failed to delete {} secret: {:?}", label, e);
        }
    }

    pub fn save_password(
        &self,
        profile: &ConnectionProfile,
        password: &SecretString,
    ) -> Result<(), crate::DbError> {
        ProfileSecrets::new(&self.secret_store).save_password(profile, password)
    }
}

impl SecretManager {
    pub fn delete_password(&self, profile: &ConnectionProfile) {
        ProfileSecrets::new(&self.secret_store).delete_password(profile)
    }

    /// The password saved for `profile`. `Ok(None)` means no password is
    /// saved (or no keyring exists to hold one); `Err` means the keyring holds
    /// an entry it would not hand over, for example because it is locked, so
    /// callers must not treat it as an absent password.
    pub fn get_password(
        &self,
        profile: &ConnectionProfile,
    ) -> Result<Option<SecretString>, crate::DbError> {
        let store = self.store_read();

        if !store.is_available() {
            return Ok(None);
        }

        store.get(&profile.secret_ref()).inspect_err(|e| {
            error!("Failed to get password: {:?}", e);
        })
    }

    pub fn get_ssh_password(&self, profile: &ConnectionProfile) -> Option<SecretString> {
        let store = self.store_read();

        if !store.is_available() {
            return None;
        }

        match store.get(&profile.ssh_secret_ref()) {
            Ok(secret) => secret,
            Err(e) => {
                error!("Failed to get SSH secret: {:?}", e);
                None
            }
        }
    }

    pub fn save_ssh_password(&self, profile: &ConnectionProfile, secret: &SecretString) {
        ProfileSecrets::new(&self.secret_store).save_ssh_password(profile, secret)
    }

    pub fn delete_ssh_password(&self, profile: &ConnectionProfile) {
        ProfileSecrets::new(&self.secret_store).delete_ssh_password(profile)
    }

    pub fn get_ssh_tunnel_secret(&self, tunnel: &SshTunnelProfile) -> Option<SecretString> {
        self.get_secret(tunnel, "SSH tunnel")
    }

    pub fn save_ssh_tunnel_secret(&self, tunnel: &SshTunnelProfile, secret: &SecretString) {
        self.save_secret(tunnel, secret, "SSH tunnel");
    }

    pub fn delete_ssh_tunnel_secret(&self, tunnel: &SshTunnelProfile) {
        self.delete_secret(tunnel, "SSH tunnel");
    }

    pub fn get_proxy_secret(&self, proxy: &ProxyProfile) -> Option<SecretString> {
        self.get_secret(proxy, "proxy")
    }

    pub fn save_proxy_secret(&self, proxy: &ProxyProfile, secret: &SecretString) {
        self.save_secret(proxy, secret, "proxy");
    }

    pub fn delete_proxy_secret(&self, proxy: &ProxyProfile) {
        self.delete_secret(proxy, "proxy");
    }

    pub fn get_proxy_secret_for_profile(
        &self,
        profile: &ConnectionProfile,
        proxies: &[ProxyProfile],
    ) -> Option<SecretString> {
        let proxy_id = profile.proxy_profile_id?;
        let proxy = proxies.iter().find(|p| p.id == proxy_id)?;

        if !proxy.enabled {
            return None;
        }

        if !proxy.save_secret {
            return None;
        }

        self.get_proxy_secret(proxy)
    }

    pub fn get_ssh_secret_for_profile(
        &self,
        profile: &ConnectionProfile,
        ssh_tunnels: &[SshTunnelProfile],
    ) -> Option<SecretString> {
        let (ssh_tunnel, ssh_tunnel_profile_id) = match &profile.config {
            DbConfig::Postgres {
                ssh_tunnel,
                ssh_tunnel_profile_id,
                ..
            } => (ssh_tunnel.as_ref(), *ssh_tunnel_profile_id),
            DbConfig::MySQL {
                ssh_tunnel,
                ssh_tunnel_profile_id,
                ..
            } => (ssh_tunnel.as_ref(), *ssh_tunnel_profile_id),
            DbConfig::MongoDB {
                ssh_tunnel,
                ssh_tunnel_profile_id,
                ..
            } => (ssh_tunnel.as_ref(), *ssh_tunnel_profile_id),
            DbConfig::Redis {
                ssh_tunnel,
                ssh_tunnel_profile_id,
                ..
            } => (ssh_tunnel.as_ref(), *ssh_tunnel_profile_id),
            DbConfig::SqlServer {
                ssh_tunnel,
                ssh_tunnel_profile_id,
                ..
            } => (ssh_tunnel.as_ref(), *ssh_tunnel_profile_id),
            DbConfig::Redshift {
                ssh_tunnel,
                ssh_tunnel_profile_id,
                ..
            } => (ssh_tunnel.as_ref(), *ssh_tunnel_profile_id),
            DbConfig::Cassandra {
                ssh_tunnel,
                ssh_tunnel_profile_id,
                ..
            } => (ssh_tunnel.as_ref(), *ssh_tunnel_profile_id),
            DbConfig::SQLite { .. }
            | DbConfig::InfluxDB { .. }
            | DbConfig::ClickHouse { .. }
            | DbConfig::Turso { .. }
            | DbConfig::DuckDB { .. }
            | DbConfig::External { .. } => {
                return None;
            }
        };

        if let Some(tunnel_profile_id) = ssh_tunnel_profile_id {
            let tunnel = ssh_tunnels.iter().find(|t| t.id == tunnel_profile_id)?;

            if !tunnel.save_secret {
                return None;
            }

            return self.get_ssh_tunnel_secret(tunnel);
        }

        if ssh_tunnel.is_some() {
            return self.get_ssh_password(profile);
        }

        None
    }
}

/// Whether the keyring is where `profile` keeps its password: the profile
/// saves it and no value ref (environment, secret manager, ...) supplies it.
pub fn password_lives_in_keyring(profile: &ConnectionProfile) -> bool {
    profile.save_password && !profile.value_refs.contains_key("password")
}

/// The keyring password a connect hands the driver.
///
/// `read` runs only when the keyring is where the profile keeps its password:
/// a profile that does not save its password, or that takes it from a value
/// ref, gets none, and a locked keyring cannot block or leak into it. A read
/// failure fails the connect only when the driver authenticates with a
/// password; a driver without one ignores it.
pub fn keyring_password_for_connect(
    profile: &ConnectionProfile,
    driver_requires_password: bool,
    read: impl FnOnce() -> Result<Option<SecretString>, crate::DbError>,
) -> Result<Option<SecretString>, String> {
    if !password_lives_in_keyring(profile) {
        return Ok(None);
    }

    match read() {
        Ok(password) => Ok(password),
        Err(error) if driver_requires_password => Err(format!(
            "Could not read the password from the system keyring (it may be locked): {error}"
        )),
        Err(error) => {
            log::warn!(
                "Ignoring keyring read failure for profile '{}', whose driver needs no \
                 password: {error}",
                profile.name
            );
            Ok(None)
        }
    }
}

/// The profile a connect hands the driver when some of its secret form fields
/// live in the keyring: a clone with each such field read from the keyring
/// into its `External` values. `None` when no field needs a read, so the
/// caller passes the profile it has. Keep the clone to the driver call: the
/// values must not reach anything that is saved, shown or logged.
///
/// `read` runs only for a field the profile marks as kept in the keyring and
/// whose value is still empty; a value already there (typed in the form, or
/// resolved from a value source) wins. A field whose entry is missing or
/// cannot be read fails the connect with a reason naming the field's label
/// from `form`, so the driver is never called without it.
pub fn profile_with_keyring_fields(
    profile: &ConnectionProfile,
    form: &crate::DriverFormDef,
    read: impl Fn(&str) -> Result<Option<SecretString>, crate::DbError>,
) -> Result<Option<ConnectionProfile>, String> {
    if matches!(profile.config, DbConfig::ClickHouse { .. })
        && [
            "ssl_mode",
            "ssl_root_cert_path",
            "ssl_client_cert_path",
            "ssl_client_key_path",
        ]
        .iter()
        .any(|field| profile.keyring_secret_fields.contains(*field))
    {
        return Err(
            "ClickHouse TLS settings are literal paths and modes, never keyring fields".into(),
        );
    }
    let DbConfig::External { values, .. } = &profile.config else {
        return Ok(None);
    };
    let pending: Vec<&String> = profile
        .keyring_secret_fields
        .iter()
        .filter(|field_id| values.get(*field_id).is_none_or(String::is_empty))
        .collect();
    if pending.is_empty() {
        return Ok(None);
    }

    let mut filled = profile.clone();
    let DbConfig::External { values, .. } = &mut filled.config else {
        return Ok(None);
    };
    for field_id in pending {
        let label = form
            .field(field_id)
            .map_or(field_id.as_str(), |field| field.label.as_str());
        let secret_ref = crate::connection_field_secret_ref(&profile.id, field_id);
        match read(&secret_ref) {
            Ok(Some(secret)) => {
                values.insert(field_id.clone(), secret.expose_secret().to_string());
            }
            Ok(None) => {
                return Err(format!(
                    "The secret field '{label}' has no saved value in the system keyring. \
                     Edit the connection and enter it again."
                ));
            }
            Err(error) => {
                return Err(format!(
                    "Could not read the secret field '{label}' from the system keyring \
                     (it may be locked): {error}"
                ));
            }
        }
    }
    Ok(Some(filled))
}

/// [`profile_with_keyring_fields`] reading from a shared secret store. A
/// store without a keyring behind it has no entry to give.
pub fn profile_with_keyring_fields_from(
    profile: &ConnectionProfile,
    form: &crate::DriverFormDef,
    secret_store: &RwLock<Box<dyn SecretStore>>,
) -> Result<Option<ConnectionProfile>, String> {
    profile_with_keyring_fields(profile, form, |secret_ref| {
        let store = match secret_store.read() {
            Ok(store) => store,
            Err(poison_err) => poison_err.into_inner(),
        };
        if !store.is_available() {
            return Ok(None);
        }
        store.get(secret_ref)
    })
}

#[cfg(test)]
mod keyring_password_tests {
    use super::keyring_password_for_connect;
    use crate::secrecy::{ExposeSecret, SecretString};
    use crate::{ConnectionProfile, DbConfig, DbError, ValueRef};

    #[allow(clippy::result_large_err)]
    fn locked() -> Result<Option<SecretString>, DbError> {
        Err(DbError::IoError(std::io::Error::other(
            "Cannot get secret of a locked object",
        )))
    }

    fn profile(save_password: bool) -> ConnectionProfile {
        let mut profile = ConnectionProfile::new("pg", DbConfig::default_postgres());
        profile.save_password = save_password;
        profile
    }

    #[test]
    fn a_locked_keyring_fails_a_profile_that_saves_its_password() {
        let error = keyring_password_for_connect(&profile(true), true, locked)
            .expect_err("a locked keyring is not an absent password");

        assert!(
            error.starts_with(
                "Could not read the password from the system keyring (it may be locked): "
            ),
            "unexpected error: {error}"
        );
        assert!(error.contains("locked object"), "unexpected error: {error}");
    }

    #[test]
    fn a_locked_keyring_does_not_block_a_profile_that_does_not_save_its_password() {
        let password = keyring_password_for_connect(&profile(false), true, locked)
            .expect("the keyring holds nothing this profile needs");

        assert!(password.is_none());
    }

    #[test]
    fn a_locked_keyring_does_not_block_a_password_value_ref() {
        let mut profile = profile(true);
        profile.value_refs.insert(
            "password".to_string(),
            ValueRef::Env {
                key: "PGPASSWORD".to_string(),
            },
        );

        let password = keyring_password_for_connect(&profile, true, locked)
            .expect("the value ref resolves the password elsewhere");

        assert!(password.is_none());
    }

    #[test]
    fn a_readable_keyring_returns_the_saved_password() {
        let password = keyring_password_for_connect(&profile(true), true, || {
            Ok(Some(SecretString::from("saved password")))
        })
        .expect("keyring read succeeds")
        .expect("a saved password is returned");

        assert_eq!(password.expose_secret(), "saved password");
    }

    #[test]
    fn a_locked_keyring_does_not_block_a_driver_without_a_password() {
        let password = keyring_password_for_connect(&profile(true), false, locked)
            .expect("the driver authenticates without a password");

        assert!(password.is_none());
    }

    #[test]
    fn a_stale_entry_is_not_used_by_a_profile_that_does_not_save_its_password() {
        let password = keyring_password_for_connect(&profile(false), true, || {
            Ok(Some(SecretString::from("stale password")))
        })
        .expect("keyring read succeeds");

        assert!(password.is_none());
    }

    #[test]
    fn a_keyring_the_password_does_not_live_in_is_not_read() {
        let mut value_ref = profile(true);
        value_ref
            .value_refs
            .insert("password".to_string(), ValueRef::env("PGPASSWORD"));

        for profile in [profile(false), value_ref] {
            let password = keyring_password_for_connect(&profile, true, || {
                panic!("the keyring must not be read")
            })
            .expect("nothing is read");

            assert!(password.is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrecy::ExposeSecret;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct FakeSecretStore {
        available: bool,
        fail_get: bool,
        fail_set: bool,
        values: Mutex<HashMap<String, SecretString>>,
    }

    impl SecretStore for FakeSecretStore {
        fn is_available(&self) -> bool {
            self.available
        }

        #[expect(
            clippy::unwrap_in_result,
            reason = "test fixture: a poisoned values mutex means an earlier test step already panicked; failing loudly is the poison contract"
        )]
        fn get(&self, secret_ref: &str) -> Result<Option<SecretString>, crate::DbError> {
            if self.fail_get {
                return Err(crate::DbError::IoError(std::io::Error::other(
                    "Cannot get secret of a locked object",
                )));
            }

            Ok(self
                .values
                .lock()
                .expect("fake secret store lock poisoned")
                .get(secret_ref)
                .cloned())
        }

        #[expect(
            clippy::unwrap_in_result,
            reason = "test fixture: a poisoned values mutex means an earlier test step already panicked; failing loudly is the poison contract"
        )]
        fn set(&self, secret_ref: &str, value: &SecretString) -> Result<(), crate::DbError> {
            if self.fail_set {
                return Err(crate::DbError::IoError(std::io::Error::other(
                    "fake keyring write failure",
                )));
            }

            self.values
                .lock()
                .expect("fake secret store lock poisoned")
                .insert(secret_ref.to_string(), value.clone());
            Ok(())
        }

        fn delete(&self, _secret_ref: &str) -> Result<(), crate::DbError> {
            Ok(())
        }
    }

    fn profile() -> ConnectionProfile {
        ConnectionProfile::new("test", DbConfig::default_sqlite())
    }

    #[test]
    fn save_password_persists_password_when_store_accepts_writes() {
        let profile = profile();
        let manager = SecretManager::new(Box::new(FakeSecretStore {
            available: true,
            fail_get: false,
            fail_set: false,
            values: Mutex::new(HashMap::new()),
        }));

        manager
            .save_password(&profile, &SecretString::from("new password"))
            .expect("password save succeeds");

        let saved = manager
            .get_password(&profile)
            .expect("keyring read succeeds")
            .expect("password is available after successful save");
        assert_eq!(saved.expose_secret(), "new password");
    }

    #[test]
    fn save_password_returns_error_and_preserves_existing_password_on_write_failure() {
        let profile = profile();
        let mut values = HashMap::new();
        values.insert(
            profile.secret_ref(),
            SecretString::from("previous password"),
        );
        let manager = SecretManager::new(Box::new(FakeSecretStore {
            available: true,
            fail_get: false,
            fail_set: true,
            values: Mutex::new(values),
        }));

        assert!(
            manager
                .save_password(&profile, &SecretString::from("replacement password"))
                .is_err()
        );

        let saved = manager
            .get_password(&profile)
            .expect("keyring read succeeds")
            .expect("failed save preserves the previous password");
        assert_eq!(saved.expose_secret(), "previous password");
    }

    #[test]
    fn get_password_reports_a_keyring_read_failure_as_an_error() {
        let profile = profile();
        let manager = SecretManager::new(Box::new(FakeSecretStore {
            available: true,
            fail_get: true,
            fail_set: false,
            values: Mutex::new(HashMap::new()),
        }));

        let error = manager
            .get_password(&profile)
            .expect_err("a locked keyring is not an absent password");
        assert!(
            error.to_string().contains("locked object"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn get_password_returns_none_when_no_password_is_saved() {
        let profile = profile();
        let manager = SecretManager::new(Box::new(FakeSecretStore {
            available: true,
            fail_get: false,
            fail_set: false,
            values: Mutex::new(HashMap::new()),
        }));

        assert!(
            manager
                .get_password(&profile)
                .expect("keyring read succeeds")
                .is_none()
        );
    }

    #[test]
    fn get_password_returns_none_when_the_keyring_is_unavailable() {
        let profile = profile();
        let manager = SecretManager::new(Box::new(FakeSecretStore {
            available: false,
            fail_get: true,
            fail_set: false,
            values: Mutex::new(HashMap::new()),
        }));

        assert!(
            manager
                .get_password(&profile)
                .expect("an unavailable keyring is not a read failure")
                .is_none()
        );
    }

    #[test]
    fn turso_profiles_do_not_request_ssh_secrets() {
        let profile = ConnectionProfile::new(
            "turso",
            DbConfig::Turso {
                url: "https://example.turso.io".to_string(),
            },
        );
        let manager = SecretManager::new(Box::new(FakeSecretStore {
            available: true,
            fail_get: false,
            fail_set: false,
            values: Mutex::new(HashMap::new()),
        }));

        assert!(manager.get_ssh_secret_for_profile(&profile, &[]).is_none());
    }
}

#[cfg(test)]
mod keyring_field_tests {
    use super::profile_with_keyring_fields;
    use crate::secrecy::SecretString;
    use crate::{ConnectionProfile, DbConfig, DbError, DbKind};
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn external_profile(values: &[(&str, &str)], marked: &[&str]) -> ConnectionProfile {
        let mut profile = ConnectionProfile::new_with_driver(
            "rpc",
            DbKind::Postgres,
            "rpc:fake",
            DbConfig::External {
                kind: DbKind::Postgres,
                values: values
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            },
        );
        profile.keyring_secret_fields = marked.iter().map(|field| field.to_string()).collect();
        profile
    }

    static FORM: std::sync::LazyLock<crate::DriverFormDef> = std::sync::LazyLock::new(|| {
        use crate::{FormFieldKind, FormSection, FormTab, field};
        crate::DriverFormDef {
            tabs: vec![FormTab {
                id: "main".into(),
                label: "Main".into(),
                sections: vec![FormSection {
                    title: "Auth".into(),
                    icon: None,
                    fields: vec![field("api_token", "API token", FormFieldKind::Password, "")],
                }],
            }],
        }
    });

    fn values(profile: &ConnectionProfile) -> &HashMap<String, String> {
        let DbConfig::External { values, .. } = &profile.config else {
            panic!("not an external config");
        };
        values
    }

    #[test]
    fn a_marked_field_is_read_from_the_keyring_into_a_clone() {
        let profile = external_profile(&[("host", "h")], &["api_token"]);
        let expected_ref = crate::connection_field_secret_ref(&profile.id, "api_token");
        let reads = RefCell::new(Vec::new());

        let filled = profile_with_keyring_fields(&profile, &FORM, |secret_ref| {
            reads.borrow_mut().push(secret_ref.to_string());
            Ok(Some(SecretString::from("token-value")))
        })
        .expect("keyring read succeeds")
        .expect("a clone carries the field");

        assert_eq!(reads.into_inner(), vec![expected_ref]);
        assert_eq!(
            values(&filled).get("api_token").map(String::as_str),
            Some("token-value")
        );
        assert!(
            !values(&profile).contains_key("api_token"),
            "the source stays clean"
        );
    }

    #[test]
    fn the_keyring_is_not_read_for_an_unmarked_or_filled_field() {
        for profile in [
            external_profile(&[("api_token", "")], &[]),
            external_profile(&[("api_token", "typed")], &["api_token"]),
            ConnectionProfile::new("pg", DbConfig::default_postgres()),
        ] {
            let filled = profile_with_keyring_fields(&profile, &FORM, |_| {
                panic!("the keyring must not be read")
            })
            .expect("nothing is read");
            assert!(filled.is_none());
        }
    }

    #[test]
    fn a_locked_keyring_fails_the_connect() {
        let profile = external_profile(&[], &["api_token"]);
        let error = profile_with_keyring_fields(&profile, &FORM, |_| {
            Err(DbError::IoError(std::io::Error::other("locked")))
        })
        .expect_err("a locked keyring fails the connect");
        assert!(error.contains("locked"), "{error}");
        assert!(error.contains("API token"), "{error}");
    }

    /// A marked field whose entry is gone fails the connect, naming the
    /// field, instead of connecting without it (#411 triage round 2, point 2).
    #[test]
    fn a_missing_keyring_entry_fails_the_connect() {
        let profile = external_profile(&[], &["api_token"]);
        let error = profile_with_keyring_fields(&profile, &FORM, |_| Ok(None))
            .expect_err("a missing entry fails the connect");
        assert!(error.contains("API token"), "{error}");
    }
}
