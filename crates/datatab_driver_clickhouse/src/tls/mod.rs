use std::fs::File;
use std::io::Read;

use datatab_core::{ConnectionProfile, DbConfig, DbError};
use reqwest::blocking::ClientBuilder;

const MAX_PEM_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TlsMode {
    LegacySystem,
    Disable,
    Require,
    VerifyCa,
    VerifyFull,
}

pub(crate) struct TlsOptions<'a> {
    mode: TlsMode,
    root: Option<&'a str>,
    certificate: Option<&'a str>,
    key: Option<&'a str>,
}

fn invalid(reason: &str) -> DbError {
    DbError::InvalidProfile(reason.to_string())
}

impl<'a> TlsOptions<'a> {
    /// Pure checks run before hooks, secret resolution and network access (#72).
    pub(crate) fn from_profile(profile: &'a ConnectionProfile) -> Result<Self, DbError> {
        let DbConfig::ClickHouse {
            url,
            ssl_mode,
            ssl_root_cert_path,
            ssl_client_cert_path,
            ssl_client_key_path,
            ..
        } = &profile.config
        else {
            return Err(invalid("Expected ClickHouse configuration"));
        };
        if [
            "ssl_mode",
            "ssl_root_cert_path",
            "ssl_client_cert_path",
            "ssl_client_key_path",
        ]
        .iter()
        .any(|field| {
            profile.value_refs.contains_key(*field)
                || profile.keyring_secret_fields.contains(*field)
        }) {
            return Err(invalid(
                "ClickHouse URL and TLS settings must be literal values, not value or secret references",
            ));
        }
        if ssl_mode.is_some() && profile.value_refs.contains_key("url") {
            return Err(invalid(
                "An explicit ClickHouse TLS mode requires a literal HTTP URL",
            ));
        }
        let mode = match ssl_mode.as_deref() {
            None => TlsMode::LegacySystem,
            Some("disable") => TlsMode::Disable,
            Some("require") => TlsMode::Require,
            Some("verify-ca") => TlsMode::VerifyCa,
            Some("verify-full") => TlsMode::VerifyFull,
            Some(_) => {
                return Err(invalid(
                    "Unknown or ambiguous ClickHouse TLS mode; choose disable, require, verify-ca or verify-full",
                ));
            }
        };
        super::http::ClickHouseHttpClient::validate_endpoint(url)
            .map_err(|_| invalid("Invalid ClickHouse HTTP URL"))?;
        let endpoint =
            reqwest::Url::parse(url).map_err(|_| invalid("Invalid ClickHouse HTTP URL"))?;
        if (mode == TlsMode::Disable && endpoint.scheme() != "http")
            || (matches!(
                mode,
                TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull
            ) && endpoint.scheme() != "https")
        {
            return Err(invalid(
                "ClickHouse disable requires http://; require, verify-ca and verify-full require https://",
            ));
        }
        let options = Self {
            mode,
            root: ssl_root_cert_path.as_deref(),
            certificate: ssl_client_cert_path.as_deref(),
            key: ssl_client_key_path.as_deref(),
        };
        options.validate_paths()?;
        Ok(options)
    }

    fn validate_paths(&self) -> Result<(), DbError> {
        for path in [self.root, self.certificate, self.key]
            .into_iter()
            .flatten()
        {
            if path.trim().is_empty() || path.contains('\0') {
                return Err(invalid(
                    "ClickHouse certificate paths must be non-empty file paths",
                ));
            }
        }
        let verifies = matches!(self.mode, TlsMode::VerifyCa | TlsMode::VerifyFull);
        if verifies && self.root.is_none() {
            return Err(invalid(
                "ClickHouse verify-ca and verify-full require a root CA path",
            ));
        }
        if !verifies && self.root.is_some() {
            return Err(invalid(
                "The chosen ClickHouse TLS mode does not use a root CA path",
            ));
        }
        if self.certificate.is_some() != self.key.is_some() {
            return Err(invalid(
                "ClickHouse mutual TLS requires both a client certificate and key path",
            ));
        }
        if self.certificate.is_some()
            && matches!(self.mode, TlsMode::Disable | TlsMode::LegacySystem)
        {
            return Err(invalid(
                "ClickHouse client certificates require an explicit TLS mode",
            ));
        }
        Ok(())
    }

    fn verification_flags(&self) -> (bool, bool, bool) {
        (
            self.mode == TlsMode::LegacySystem,
            self.mode == TlsMode::Require,
            self.mode == TlsMode::VerifyCa,
        )
    }

    pub(crate) fn configure(&self, builder: ClientBuilder) -> Result<ClientBuilder, DbError> {
        // Explicit modes never load the system store. Verification modes trust
        // only the supplied CA; require deliberately does not verify certificates.
        let (built_in_root_certs, accept_invalid_certs, accept_invalid_hostnames) =
            self.verification_flags();
        let mut builder = builder
            .tls_built_in_root_certs(built_in_root_certs)
            .danger_accept_invalid_certs(accept_invalid_certs)
            .danger_accept_invalid_hostnames(accept_invalid_hostnames);
        if let Some(path) = self.root {
            let pem = read_pem(path, "root CA")?;
            let certificates = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|_| invalid("Invalid ClickHouse root CA PEM"))?;
            if certificates.is_empty() {
                return Err(invalid("ClickHouse root CA PEM contains no certificates"));
            }
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        if let (Some(certificate), Some(key)) = (self.certificate, self.key) {
            let mut pem = read_pem(certificate, "client certificate")?;
            pem.push(b'\n');
            pem.extend(read_pem(key, "client key")?);
            let identity = reqwest::Identity::from_pem(&pem)
                .map_err(|_| invalid("Invalid ClickHouse client identity PEM; use an unencrypted matching certificate and key"))?;
            builder = builder.identity(identity);
        }
        Ok(builder)
    }
}

fn read_pem(path: &str, kind: &str) -> Result<Vec<u8>, DbError> {
    let file =
        File::open(path).map_err(|_| invalid(&format!("Cannot open ClickHouse {kind} file")))?;
    let mut bytes = Vec::new();
    file.take(MAX_PEM_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid(&format!("Cannot read ClickHouse {kind} file")))?;
    if bytes.len() as u64 > MAX_PEM_BYTES {
        return Err(invalid(
            "ClickHouse certificate and key files must not exceed 1 MiB each",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
