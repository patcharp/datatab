use redis::{Client, Connection, ConnectionAddr, ConnectionInfo, RedisConnectionInfo};
use redis::{ErrorKind, RedisError, RedisResult, TlsCertificates};

const MAX_ANNOUNCED_HOST_LENGTH: usize = 253;

/// TLS-only Sentinel discovery. Plain Sentinel keeps redis-rs's resolver.
pub(crate) struct TlsSentinelResolver {
    nodes: Vec<Client>,
    master_name: String,
    master_credentials: RedisConnectionInfo,
    insecure: bool,
    certificates: TlsCertificates,
}

impl TlsSentinelResolver {
    pub(crate) fn new(
        primary: (&str, u16),
        additional_nodes: &[(String, u16)],
        master_name: &str,
        master_credentials: RedisConnectionInfo,
        insecure: bool,
        certificates: TlsCertificates,
    ) -> RedisResult<Self> {
        let nodes = std::iter::once(primary)
            .chain(
                additional_nodes
                    .iter()
                    .map(|(host, port)| (host.as_str(), *port)),
            )
            .map(|(host, port)| {
                tls_client(
                    host,
                    port,
                    insecure,
                    RedisConnectionInfo::default(),
                    &certificates,
                )
            })
            .collect::<RedisResult<Vec<_>>>()?;
        Ok(Self {
            nodes,
            master_name: master_name.to_string(),
            master_credentials,
            insecure,
            certificates,
        })
    }

    pub(crate) fn get_connection(&mut self) -> RedisResult<(Connection, (String, u16))> {
        let (host, port) = self.resolve_master()?;
        let connection = self.connect_master(&host, port).map_err(|error| {
            RedisError::from((
                error.kind(),
                "Redis Sentinel master connection failed",
                format!("master {host}:{port}: {error}"),
            ))
        })?;
        Ok((connection, (host, port)))
    }

    fn connect_master(&self, host: &str, port: u16) -> RedisResult<Connection> {
        let client = tls_client(
            host,
            port,
            self.insecure,
            self.master_credentials.clone(),
            &self.certificates,
        )?;
        let mut connection = client.get_connection()?;
        let role = redis::cmd("ROLE").query::<redis::Role>(&mut connection)?;
        require_master(&role)?;
        Ok(connection)
    }

    fn resolve_master(&self) -> RedisResult<(String, u16)> {
        let mut last_error = None;
        for node in &self.nodes {
            let result = node.get_connection().and_then(|mut connection| {
                redis::cmd("SENTINEL")
                    .arg("GET-MASTER-ADDR-BY-NAME")
                    .arg(&self.master_name)
                    .query::<Option<(String, u16)>>(&mut connection)
            });
            match result {
                Ok(Some((host, port)))
                    if !host.is_empty() && host.len() <= MAX_ANNOUNCED_HOST_LENGTH && port != 0 =>
                {
                    return Ok((host, port));
                }
                Ok(_) => {
                    last_error = Some(missing_master_error(last_error));
                }
                Err(error) => last_error = Some(node_error(node, error)),
            }
        }
        Err(last_error.unwrap_or_else(|| missing_master_error(None)))
    }
}

fn node_error(node: &Client, error: RedisError) -> RedisError {
    let address = match &node.get_connection_info().addr {
        ConnectionAddr::TcpTls { host, port, .. } => format!("{host}:{port}"),
        _ => "unknown".to_string(),
    };
    RedisError::from((
        error.kind(),
        "Redis Sentinel node connection failed",
        format!("Sentinel {address}: {error}"),
    ))
}

fn missing_master_error(previous: Option<RedisError>) -> RedisError {
    let message = "Redis Sentinel did not return a master address";
    match previous {
        Some(error) if error.kind() != ErrorKind::MasterNameNotFoundBySentinel => {
            RedisError::from((
                ErrorKind::MasterNameNotFoundBySentinel,
                message,
                format!("Earlier Sentinel result: {error}"),
            ))
        }
        Some(error) => error,
        None => RedisError::from((ErrorKind::MasterNameNotFoundBySentinel, message)),
    }
}

fn tls_client(
    host: &str,
    port: u16,
    insecure: bool,
    credentials: RedisConnectionInfo,
    certificates: &TlsCertificates,
) -> RedisResult<Client> {
    // Never interpolate an announced host into a URI: fragments could change
    // verification, and IPv6 addresses would need different escaping.
    Client::build_with_tls(
        ConnectionInfo {
            addr: ConnectionAddr::TcpTls {
                host: host.to_string(),
                port,
                insecure,
                tls_params: None,
            },
            redis: credentials,
        },
        certificates.clone(),
    )
}

fn require_master(role: &redis::Role) -> RedisResult<()> {
    if matches!(role, redis::Role::Primary { .. }) {
        Ok(())
    } else {
        Err(RedisError::from((
            ErrorKind::ResponseError,
            "Redis Sentinel resolved a non-master; failover in progress, retry",
        )))
    }
}

#[cfg(test)]
mod tests;
