//! TLS modes for the Cassandra session.
//!
//! The four modes use the words the other drivers use (ruling 7):
//! `disable`, `require`, `verify-ca` and `verify-full`. `require` encrypts
//! without verifying anything and says so; `verify-ca` checks the chain only,
//! which is the practical mode because the driver checks each node's IP;
//! `verify-full` also checks the node name and therefore needs an IP SAN on
//! every node's certificate.

use std::io::BufReader;
use std::sync::Arc;

use datatab_core::DbError;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TlsMode {
    Disable,
    Require,
    VerifyCa,
    VerifyFull,
}

impl TlsMode {
    pub(crate) fn parse(id: &str) -> Result<Self, DbError> {
        match id {
            "disable" => Ok(Self::Disable),
            "require" => Ok(Self::Require),
            "verify-ca" => Ok(Self::VerifyCa),
            "verify-full" => Ok(Self::VerifyFull),
            other => Err(DbError::InvalidProfile(format!(
                "Unknown TLS mode '{other}'; use disable, require, verify-ca or verify-full"
            ))),
        }
    }

    pub(crate) fn as_id(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }

    pub(crate) fn requires_root_cert(self) -> bool {
        matches!(self, Self::VerifyCa | Self::VerifyFull)
    }
}

/// Builds the rustls configuration for a mode. `None` turns TLS off.
///
/// A client certificate and key turn on mutual TLS: the cluster then
/// authenticates the client as well as the client authenticating the cluster.
/// The two paths travel together; a profile that sets only one of them is
/// refused before this runs.
pub(crate) fn client_config(
    mode: TlsMode,
    ca_path: Option<&str>,
    client_cert_path: Option<&str>,
    client_key_path: Option<&str>,
) -> Result<Option<Arc<ClientConfig>>, DbError> {
    if mode == TlsMode::Disable {
        return Ok(None);
    }

    let provider = CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    let client_auth = load_client_auth(client_cert_path, client_key_path)?;

    // Each mode decides how the node's certificate is verified; every mode
    // then applies the same client-auth choice.
    let verifier_builder = if mode == TlsMode::Require {
        ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(tls_error)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerification { provider }))
    } else {
        let roots = load_roots(ca_path)?;
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(tls_error)?;
        match mode {
            TlsMode::VerifyFull => builder.with_root_certificates(roots),
            TlsMode::VerifyCa => builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(ChainOnlyVerifier {
                    provider,
                    roots: Arc::new(roots),
                })),
            // Both are decided above, so this cannot happen; an error keeps a
            // future mode from panicking inside a TLS setup.
            TlsMode::Disable | TlsMode::Require => {
                return Err(tls_error(format!(
                    "TLS mode '{}' reached the verifying branch",
                    mode.as_id()
                )));
            }
        }
    };

    let config = match client_auth {
        Some((chain, key)) => verifier_builder
            .with_client_auth_cert(chain, key)
            .map_err(tls_error)?,
        None => verifier_builder.with_no_client_auth(),
    };
    Ok(Some(Arc::new(config)))
}

/// Reads the client certificate chain and its private key.
///
/// Connected clusters often run without mutual TLS, so no paths means no
/// client certificate. One path without the other is a profile mistake that
/// `validate_profile` already refuses; it is refused here too so a direct
/// caller cannot half-configure the handshake.
fn load_client_auth(
    cert_path: Option<&str>,
    key_path: Option<&str>,
) -> Result<Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>, DbError> {
    let (cert_path, key_path) = match (cert_path, key_path) {
        (None, None) => return Ok(None),
        (Some(cert), Some(key)) => (cert, key),
        (Some(_), None) => {
            return Err(DbError::InvalidProfile(
                "A client certificate also needs its private key".into(),
            ));
        }
        (None, Some(_)) => {
            return Err(DbError::InvalidProfile(
                "A client key also needs its client certificate".into(),
            ));
        }
    };

    let file = std::fs::File::open(cert_path).map_err(|error| {
        DbError::connection_failed(format!(
            "Cannot read the client certificate '{cert_path}': {error}"
        ))
    })?;
    let mut reader = BufReader::new(file);
    let mut chain = Vec::new();
    for certificate in rustls_pemfile::certs(&mut reader) {
        let certificate = certificate.map_err(|error| {
            DbError::connection_failed(format!(
                "The client certificate '{cert_path}' does not hold PEM certificates: {error}"
            ))
        })?;
        chain.push(certificate);
    }
    if chain.is_empty() {
        return Err(DbError::connection_failed(format!(
            "The client certificate '{cert_path}' holds no certificate"
        )));
    }

    let file = std::fs::File::open(key_path).map_err(|error| {
        DbError::connection_failed(format!("Cannot read the client key '{key_path}': {error}"))
    })?;
    let mut reader = BufReader::new(file);
    let key = rustls_pemfile::private_key(&mut reader)
        .map_err(|error| {
            DbError::connection_failed(format!(
                "The client key '{key_path}' does not hold a PEM private key: {error}"
            ))
        })?
        .ok_or_else(|| {
            DbError::connection_failed(format!("The client key '{key_path}' holds no private key"))
        })?;

    Ok(Some((chain, key)))
}

/// A private CA file when the profile names one, the platform roots otherwise.
fn load_roots(ca_path: Option<&str>) -> Result<RootCertStore, DbError> {
    let mut roots = RootCertStore::empty();
    match ca_path {
        Some(path) => {
            let file = std::fs::File::open(path).map_err(|error| {
                DbError::connection_failed(format!("Cannot read the CA file '{path}': {error}"))
            })?;
            let mut reader = BufReader::new(file);
            let mut added = 0_usize;
            for certificate in rustls_pemfile::certs(&mut reader) {
                let certificate = certificate.map_err(|error| {
                    DbError::connection_failed(format!(
                        "The CA file '{path}' does not hold PEM certificates: {error}"
                    ))
                })?;
                roots.add(certificate).map_err(|error| {
                    DbError::connection_failed(format!(
                        "The CA file '{path}' holds a certificate rustls rejected: {error}"
                    ))
                })?;
                added += 1;
            }
            if added == 0 {
                return Err(DbError::connection_failed(format!(
                    "The CA file '{path}' holds no certificate"
                )));
            }
        }
        None => {
            let native = rustls_native_certs::load_native_certs();
            for error in &native.errors {
                log::warn!("Cassandra TLS: a platform root could not be read: {error}");
            }
            for certificate in native.certs {
                roots.add(certificate).map_err(tls_error)?;
            }
            if roots.is_empty() {
                return Err(DbError::connection_failed(
                    "No platform root certificate is available; set a CA certificate",
                ));
            }
        }
    }
    Ok(roots)
}

fn tls_error(error: impl std::fmt::Display) -> DbError {
    DbError::connection_failed(format!("Cassandra TLS setup failed: {error}"))
}

/// Chain verification only: the certificate must chain to a trusted root and
/// be valid now, and the node name is not checked.
#[derive(Debug)]
struct ChainOnlyVerifier {
    provider: Arc<CryptoProvider>,
    roots: Arc<RootCertStore>,
}

impl ServerCertVerifier for ChainOnlyVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let certificate = rustls::server::ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_cert_signed_by_trust_anchor(
            &certificate,
            &self.roots,
            intermediates,
            now,
            self.provider.signature_verification_algorithms.all,
        )?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// `require`: encryption with no verification at all. A profile reaches this
/// only by choosing the mode, and the README says what it gives up.
#[derive(Debug)]
struct NoVerification {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for NoVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _certificate: &CertificateDer<'_>,
        _signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _certificate: &CertificateDer<'_>,
        _signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
