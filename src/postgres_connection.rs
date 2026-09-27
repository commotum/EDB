pub(crate) mod environment;

use crate::{ErrorCategory, SemanticError};
use native_tls::{Certificate, Protocol, TlsConnector};
use postgres::config::{Host, SslMode};
use postgres::{Client, Config, NoTls};
use postgres_native_tls::{MakeTlsConnector, set_postgresql_alpn};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

mod error;
pub(crate) use error::{is_postgres_connection_error, postgres_error};

/// Explicit per-connection I/O settings shared by plaintext and verified TLS.
/// `None` preserves the parameter string/driver/server default. There is no
/// implicit production timeout: administrative and runtime roles may need
/// different policies.
///
/// SQL timeouts are server-side and apply to each statement or lock wait,
/// not the complete transaction or arbitrary Rust code. Connection timeout
/// caps each socket/address attempt, not DNS, TLS/authentication/startup, or
/// the complete multi-host operation. TCP controls are OS-dependent and have
/// no effect on Unix-domain sockets; they are not a measured outage bound.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PostgresIoPolicy {
    /// Upper bound per socket/address connection attempt, before protocol startup.
    pub connect_timeout: Option<Duration>,
    /// Server-side limit per SQL statement. Positive milliseconds, rounded up.
    pub statement_timeout: Option<Duration>,
    /// Server-side limit per lock wait. Positive milliseconds, rounded up.
    pub lock_timeout: Option<Duration>,
    /// OS-dependent bound on unacknowledged TCP data, not a query deadline.
    pub tcp_user_timeout: Option<Duration>,
    /// Enable or disable TCP keepalives; ignored for Unix-domain sockets.
    pub keepalives: Option<bool>,
    /// Idle time before keepalive probes. Positive seconds, rounded up.
    pub keepalives_idle: Option<Duration>,
    /// Interval between keepalive probes. Positive seconds, rounded up.
    pub keepalives_interval: Option<Duration>,
    /// Maximum unanswered keepalive probes; the OS may impose tighter limits.
    pub keepalives_retries: Option<u32>,
}

impl PostgresIoPolicy {
    fn validate(&self) -> Result<(), SemanticError> {
        for (field, duration) in [
            ("connect_timeout", self.connect_timeout),
            ("statement_timeout", self.statement_timeout),
            ("lock_timeout", self.lock_timeout),
            ("tcp_user_timeout", self.tcp_user_timeout),
            ("keepalives_idle", self.keepalives_idle),
            ("keepalives_interval", self.keepalives_interval),
        ] {
            if duration.is_some_and(|duration| duration.is_zero()) {
                return Err(invalid_io_policy(field));
            }
        }
        for (field, duration) in [
            ("statement_timeout", self.statement_timeout),
            ("lock_timeout", self.lock_timeout),
            ("tcp_user_timeout", self.tcp_user_timeout),
        ] {
            if let Some(duration) = duration {
                policy_milliseconds(field, duration)?;
            }
        }
        for (field, duration) in [
            ("keepalives_idle", self.keepalives_idle),
            ("keepalives_interval", self.keepalives_interval),
        ] {
            if let Some(duration) = duration {
                policy_seconds(field, duration)?;
            }
        }
        if self
            .keepalives_retries
            .is_some_and(|retries| retries == 0 || retries > i32::MAX as u32)
        {
            return Err(invalid_io_policy("keepalives_retries"));
        }
        Ok(())
    }
}

fn invalid_io_policy(field: &'static str) -> SemanticError {
    SemanticError::incorrect(
        "postgres/invalid-io-policy",
        "PostgreSQL I/O policy values must be positive and within the supported setting range",
    )
    .detail("field", field)
}

fn policy_milliseconds(field: &'static str, duration: Duration) -> Result<u64, SemanticError> {
    let milliseconds = duration.as_nanos().div_ceil(1_000_000);
    if milliseconds == 0 || milliseconds > i32::MAX as u128 {
        return Err(invalid_io_policy(field));
    }
    Ok(milliseconds as u64)
}

fn policy_seconds(field: &'static str, duration: Duration) -> Result<u64, SemanticError> {
    let seconds = duration.as_nanos().div_ceil(1_000_000_000);
    if seconds == 0 || seconds > i32::MAX as u128 {
        return Err(invalid_io_policy(field));
    }
    Ok(seconds as u64)
}

/// One concrete PostgreSQL connection policy shared by every runtime role.
///
/// The parsed driver configuration owns the transport policy. TCP connections
/// require verified TLS unless `sslmode=disable` explicitly selects plaintext;
/// local Unix sockets need no TLS. Explicit builders reject conflicting DSN
/// settings. TLS 1.2, certificate and hostname verification remain mandatory.
#[derive(Clone, Eq, PartialEq)]
pub struct PostgresConnectionConfig {
    parameters: Arc<str>,
    parsed: Result<tokio_postgres::Config, SemanticError>,
    root_certificates: Arc<[Vec<u8>]>,
    io_policy: PostgresIoPolicy,
    ssd_cache: Option<crate::SsdCacheConfig>,
}

impl fmt::Debug for PostgresConnectionConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresConnectionConfig")
            .field(
                "transport",
                &if self.tls_required() {
                    "verified-tls"
                } else {
                    "plaintext"
                },
            )
            .field("custom_root_certificates", &self.root_certificates.len())
            .field("io_policy", &self.io_policy)
            .field("ssd_cache_enabled", &self.ssd_cache.is_some())
            .finish_non_exhaustive()
    }
}

impl PostgresConnectionConfig {
    /// Parse the DSN once, using verified TLS for TCP unless explicitly disabled.
    pub fn parse(parameters: impl Into<String>) -> Result<Self, SemanticError> {
        let config = Self::from_parameters(parameters.into(), None);
        config.parsed.as_ref().map_err(Clone::clone)?;
        Ok(config)
    }

    /// Explicitly select plaintext, rejecting a DSN that requires TLS.
    pub fn plaintext(parameters: impl Into<String>) -> Self {
        Self::from_parameters(parameters.into(), Some(SslMode::Disable))
    }

    /// Require verified TLS, rejecting a DSN that explicitly disables it.
    pub fn require_tls(parameters: impl Into<String>) -> Self {
        Self::from_parameters(parameters.into(), Some(SslMode::Require))
    }

    fn from_parameters(parameters: String, requested: Option<SslMode>) -> Self {
        let parsed = parameters
            .parse::<tokio_postgres::Config>()
            .map_err(|_| {
                SemanticError::incorrect(
                    "postgres/invalid-connection-config",
                    "invalid PostgreSQL connection configuration",
                )
            })
            .and_then(|mut config| {
                let mode = config.get_ssl_mode();
                if requested.is_some_and(|requested| mode != SslMode::Prefer && mode != requested) {
                    return Err(SemanticError::incorrect(
                        "postgres/conflicting-transport",
                        "PostgreSQL sslmode conflicts with the explicit transport selection",
                    ));
                }
                let mode = requested.unwrap_or_else(|| match mode {
                    SslMode::Prefer
                        if config.get_hostaddrs().is_empty()
                            && (config.get_hosts().is_empty()
                                || config
                                    .get_hosts()
                                    .iter()
                                    .all(|host| matches!(host, Host::Unix(_)))) =>
                    {
                        SslMode::Disable
                    }
                    SslMode::Prefer => SslMode::Require,
                    mode => mode,
                });
                config.ssl_mode(mode);
                Ok(config)
            });
        Self {
            parameters: Arc::from(parameters),
            parsed,
            root_certificates: Arc::default(),
            io_policy: PostgresIoPolicy::default(),
            ssd_cache: None,
        }
    }

    /// Add a PEM-encoded trust root used in addition to platform roots.
    ///
    /// This accepts one certificate per call so callers can construct an
    /// explicit trust set.
    pub fn with_root_certificate_pem(
        mut self,
        certificate_pem: impl AsRef<[u8]>,
    ) -> Result<Self, SemanticError> {
        self.parsed.as_ref().map_err(Clone::clone)?;
        if !self.tls_required() {
            return Err(SemanticError::incorrect(
                "postgres/tls-not-enabled",
                "root certificates require a TLS-enabled PostgreSQL connection",
            ));
        }
        let certificate_pem = certificate_pem.as_ref();
        Certificate::from_pem(certificate_pem).map_err(|_| invalid_root_certificate())?;
        let mut updated = self.root_certificates.iter().cloned().collect::<Vec<_>>();
        updated.push(certificate_pem.to_vec());
        self.root_certificates = updated.into();
        Ok(self)
    }

    pub fn tls_required(&self) -> bool {
        self.parsed
            .as_ref()
            .is_ok_and(|config| config.get_ssl_mode() == SslMode::Require)
    }

    /// Enable disposable local block reuse. Opening a native peer validates
    /// this private, pre-existing directory; PostgreSQL still authorizes it.
    pub fn with_ssd_cache(mut self, config: crate::SsdCacheConfig) -> Self {
        self.ssd_cache = Some(config);
        self
    }

    pub fn ssd_cache_config(&self) -> Option<&crate::SsdCacheConfig> {
        self.ssd_cache.as_ref()
    }
    pub(crate) fn without_ssd_cache(mut self) -> Self {
        self.ssd_cache = None;
        self
    }

    /// Opaque access/format separation, not an authorization credential.
    /// Different raw connection parameters/trust roots intentionally cannot
    /// reuse each other's cache entries, even if they name the same server.
    pub(crate) fn ssd_access_namespace(&self, lineage: &str) -> crate::Digest {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"edb/ssd-access/canonical-node-v1/physical-envelope-v1\0");
        let mut field = |bytes: &[u8]| {
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        };
        field(self.parameters.as_bytes());
        field(lineage.as_bytes());
        field(if self.tls_required() {
            b"verified-tls"
        } else {
            b"plaintext"
        });
        for root in self.root_certificates.iter() {
            field(root);
        }
        digest.finalize().into()
    }

    /// Apply explicit settings to every connection/reconnection using this
    /// configuration. SQL settings override corresponding startup options;
    /// unrelated options are retained. Socket connection caps only tighten
    /// the parameter string's existing cap. Fractional SQL/TCP milliseconds
    /// and keepalive seconds round upward, never to disabling zero.
    pub fn with_io_policy(mut self, policy: PostgresIoPolicy) -> Result<Self, SemanticError> {
        policy.validate()?;
        self.io_policy = policy;
        Ok(self)
    }

    pub fn io_policy(&self) -> &PostgresIoPolicy {
        &self.io_policy
    }

    fn prepared_config(
        &self,
        operation: &'static str,
        timeout: Option<Duration>,
    ) -> Result<tokio_postgres::Config, SemanticError> {
        let mut config = self
            .parsed
            .clone()
            .map_err(|error| error.detail("operation", operation))?;
        if timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err(SemanticError::new(
                ErrorCategory::Unavailable,
                operation,
                "PostgreSQL connection deadline elapsed",
            )
            .detail("postgres_transport", "true"));
        }
        let cap = [
            config.get_connect_timeout().copied(),
            self.io_policy.connect_timeout,
            timeout,
        ]
        .into_iter()
        .flatten()
        .min();
        if let Some(cap) = cap {
            config.connect_timeout(cap);
        }
        if let Some(timeout) = self.io_policy.tcp_user_timeout {
            config.tcp_user_timeout(Duration::from_millis(policy_milliseconds(
                "tcp_user_timeout",
                timeout,
            )?));
        }
        if let Some(enabled) = self.io_policy.keepalives {
            config.keepalives(enabled);
        }
        if let Some(duration) = self.io_policy.keepalives_idle {
            config.keepalives_idle(Duration::from_secs(policy_seconds(
                "keepalives_idle",
                duration,
            )?));
        }
        if let Some(duration) = self.io_policy.keepalives_interval {
            config.keepalives_interval(Duration::from_secs(policy_seconds(
                "keepalives_interval",
                duration,
            )?));
        }
        if let Some(retries) = self.io_policy.keepalives_retries {
            config.keepalives_retries(retries);
        }
        let mut options = config.get_options().unwrap_or_default().to_owned();
        for (name, duration) in [
            ("statement_timeout", self.io_policy.statement_timeout),
            ("lock_timeout", self.io_policy.lock_timeout),
        ] {
            if let Some(duration) = duration {
                let milliseconds = policy_milliseconds(name, duration)?;
                use std::fmt::Write;
                write!(&mut options, " -c {name}={milliseconds}ms")
                    .expect("writing into a String cannot fail");
            }
        }
        if self.io_policy.statement_timeout.is_some() || self.io_policy.lock_timeout.is_some() {
            config.options(&options);
        }
        Ok(config)
    }

    /// Open a caller-owned PostgreSQL client under this transport policy.
    pub fn connect(&self) -> Result<Client, SemanticError> {
        self.connect_raw_for_with_timeout("postgres/connect", None)
    }

    pub(crate) fn connect_for(
        &self,
        operation: &'static str,
    ) -> Result<crate::sql_io::SqlClient, SemanticError> {
        self.connect_for_with_timeout(operation, None)
    }

    /// Cap each socket-level attempt by the smallest configured/policy/caller
    /// timeout. This never lengthens an existing cap. DNS, protocol startup,
    /// authentication/TLS, and multiple address attempts are not a complete
    /// wall-clock deadline; see [`PostgresIoPolicy`].
    pub(crate) fn connect_for_with_timeout(
        &self,
        operation: &'static str,
        timeout: Option<Duration>,
    ) -> Result<crate::sql_io::SqlClient, SemanticError> {
        crate::sql_io::SqlClient::connect_with(|| {
            self.connect_raw_for_with_timeout(operation, timeout)
        })
    }

    fn connect_raw_for_with_timeout(
        &self,
        operation: &'static str,
        timeout: Option<Duration>,
    ) -> Result<Client, SemanticError> {
        let (config, tls) = self.listener_connection_parts(operation, timeout)?;
        let config = Config::from(config);
        match tls {
            None => config
                .connect(NoTls)
                .map_err(|error| postgres_error(operation, error)),
            Some(connector) => config
                .connect(connector)
                .map_err(|_| Self::tls_connection_error(operation)),
        }
    }

    /// The bounded notification driver uses the same parsed connection,
    /// transport authentication, roots and I/O policy as synchronous clients.
    pub(crate) fn listener_connection_parts(
        &self,
        operation: &'static str,
        timeout: Option<Duration>,
    ) -> Result<(tokio_postgres::Config, Option<MakeTlsConnector>), SemanticError> {
        let config = self.prepared_config(operation, timeout)?;
        if config.get_ssl_mode() == SslMode::Disable {
            return Ok((config, None));
        }
        if config
            .get_hosts()
            .iter()
            .any(|host| matches!(host, Host::Unix(_)))
        {
            return Err(SemanticError::incorrect(
                "postgres/tls-requires-tcp",
                "PostgreSQL TLS requires a TCP host, not a Unix-domain socket",
            )
            .detail("operation", operation));
        }
        let mut builder = TlsConnector::builder();
        builder.min_protocol_version(Some(Protocol::Tlsv12));
        set_postgresql_alpn(&mut builder);
        for certificate_pem in self.root_certificates.iter() {
            let certificate =
                Certificate::from_pem(certificate_pem).map_err(|_| invalid_root_certificate())?;
            builder.add_root_certificate(certificate);
        }
        let connector = builder.build().map_err(|_| {
            SemanticError::incorrect(
                "postgres/invalid-tls-config",
                "invalid PostgreSQL TLS configuration",
            )
            .detail("operation", operation)
        })?;
        Ok((config, Some(MakeTlsConnector::new(connector))))
    }

    pub(crate) fn tls_connection_error(operation: &'static str) -> SemanticError {
        SemanticError::new(
            ErrorCategory::Unavailable,
            "postgres/tls-connect",
            "verified PostgreSQL TLS connection failed",
        )
        .detail("operation", operation)
        .detail("postgres_transport", "true")
    }
}

fn invalid_root_certificate() -> SemanticError {
    SemanticError::incorrect(
        "postgres/invalid-tls-root-certificate",
        "PostgreSQL TLS root certificate is not valid PEM",
    )
}
