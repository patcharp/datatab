use super::*;

fn certificates() -> TlsCertificates {
    TlsCertificates {
        client_tls: None,
        root_cert: None,
    }
}

#[test]
fn test_announced_host_is_a_structured_tls_address() {
    for host in ["::1", "localhost/#insecure", "localhost:6379/0?db=4"] {
        let client = tls_client(
            host,
            6380,
            false,
            RedisConnectionInfo {
                db: 2,
                username: Some("fixture-user".into()),
                ..Default::default()
            },
            &certificates(),
        )
        .expect("build structured TLS client");
        assert!(matches!(&client.get_connection_info().addr,
            ConnectionAddr::TcpTls { host: actual, port: 6380, insecure: false, tls_params: Some(_) }
                if actual == host));
        assert_eq!(client.get_connection_info().redis.db, 2);
        assert_eq!(
            client.get_connection_info().redis.username.as_deref(),
            Some("fixture-user")
        );
    }
}

#[test]
fn test_every_sentinel_node_keeps_the_same_tls_mode() {
    for insecure in [false, true] {
        let resolver = TlsSentinelResolver::new(
            ("localhost", 26379),
            &[("::1".into(), 26380), ("localhost".into(), 26381)],
            "primary",
            RedisConnectionInfo {
                db: 2,
                username: Some("master-only".into()),
                ..Default::default()
            },
            insecure,
            certificates(),
        )
        .expect("build TLS resolver");
        assert_eq!(resolver.nodes.len(), 3);
        for node in &resolver.nodes {
            let info = node.get_connection_info();
            assert!(matches!(info.addr, ConnectionAddr::TcpTls {
                insecure: actual, tls_params: Some(_), .. } if actual == insecure));
            assert!(
                info.redis.username.is_none(),
                "master credentials must not reach Sentinel nodes"
            );
            assert_eq!(info.redis.db, 0);
        }
    }
}

#[test]
fn test_role_check_requires_a_master() {
    assert!(
        require_master(&redis::Role::Primary {
            replication_offset: 0,
            replicas: Vec::new()
        })
        .is_ok()
    );
    assert!(
        require_master(&redis::Role::Replica {
            primary_ip: "127.0.0.1".into(),
            primary_port: 6379,
            replication_state: "connected".into(),
            data_received: 0
        })
        .is_err()
    );
    assert!(
        require_master(&redis::Role::Sentinel {
            primary_names: Vec::new()
        })
        .is_err()
    );
}

#[test]
fn sentinel_node_error_identifies_address() {
    let node = tls_client(
        "localhost",
        26379,
        true,
        RedisConnectionInfo::default(),
        &certificates(),
    )
    .expect("client");
    let error = RedisError::from((ErrorKind::IoError, "connection refused"));
    let message = node_error(&node, error).to_string();
    assert!(message.contains("localhost:26379"), "{message}");
}

#[test]
fn missing_master_does_not_repeat_placeholder() {
    let previous = RedisError::from((
        ErrorKind::MasterNameNotFoundBySentinel,
        "Redis Sentinel did not return a master address",
    ));
    let message = missing_master_error(Some(previous)).to_string();
    assert_eq!(
        message
            .matches("Redis Sentinel did not return a master address")
            .count(),
        1,
        "{message}"
    );
}
