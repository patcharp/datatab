use std::sync::LazyLock;

use crate::driver::DriverMetadataBuilder;
use crate::{
    Connection, ConnectionProfile, DatabaseCategory, DbConfig, DbDriver, DbError, DbKind,
    DriverFormDef, DriverKey, DriverMetadata, FormValues, QueryLanguage,
};
use secrecy::SecretString;

#[derive(Default)]
pub(super) struct TestDriver {
    pub(super) refuses_profile: bool,
}

impl DbDriver for TestDriver {
    fn validate_profile(&self, _: &ConnectionProfile) -> Result<(), DbError> {
        if self.refuses_profile {
            Err(DbError::InvalidProfile("fixture refusal".into()))
        } else {
            Ok(())
        }
    }

    fn kind(&self) -> DbKind {
        DbKind::Postgres
    }

    fn metadata(&self) -> &DriverMetadata {
        static METADATA: LazyLock<DriverMetadata> = LazyLock::new(|| {
            DriverMetadataBuilder::new(
                "pipeline-test",
                "Pipeline test",
                DatabaseCategory::Relational,
                QueryLanguage::Sql,
            )
            .build()
        });
        &METADATA
    }

    fn form_definition(&self) -> &DriverFormDef {
        static FORM: DriverFormDef = DriverFormDef { tabs: Vec::new() };
        &FORM
    }

    fn driver_key(&self) -> DriverKey {
        "builtin:pipeline-test".to_string()
    }

    fn build_config(&self, _: &FormValues) -> Result<DbConfig, DbError> {
        Err(DbError::NotSupported("pipeline test driver".into()))
    }

    fn extract_values(&self, _: &DbConfig) -> FormValues {
        FormValues::new()
    }

    fn connect_with_secrets(
        &self,
        _: &ConnectionProfile,
        _: Option<&SecretString>,
        _: Option<&SecretString>,
    ) -> Result<Box<dyn Connection>, DbError> {
        Err(DbError::NotSupported("pipeline test driver".into()))
    }

    fn test_connection(&self, _: &ConnectionProfile) -> Result<(), DbError> {
        Err(DbError::NotSupported("pipeline test driver".into()))
    }
}
