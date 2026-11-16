use super::super::McpAccessManager;
use datatab_core::DbError;
use std::collections::HashMap;

#[tokio::test]
async fn managed_access_is_unavailable() {
    let manager = McpAccessManager::new();
    let result = manager
        .open_managed("custom-provider", &HashMap::new(), "localhost")
        .await;
    let error = match result {
        Ok(_) => panic!("managed access should be unavailable"),
        Err(error) => error,
    };
    let DbError::ConnectionFailed(error) = error else {
        panic!("managed access should return a connection error");
    };
    assert_eq!(
        error.message,
        "Managed access providers are not available in this build"
    );
}
