//! Shared environment configuration for supported executables and applications.
//! Credentials stay in the caller's environment, never in command-line flags or
//! diagnostics. This selects the existing PostgreSQL transport/I/O policy.
use crate::{PostgresConnectionConfig, PostgresIoPolicy, SemanticError};
use std::time::Duration;

/// Read `EDB_POSTGRES_URL`, whose `sslmode` selects the transport. TCP
/// defaults to verified TLS; `sslmode=disable` explicitly selects plaintext. Optional
/// `EDB_POSTGRES_TLS_ROOT` supplies one additional PEM trust root.
///
/// Optional positive integer settings: `EDB_CONNECT_TIMEOUT_MS`,
/// `EDB_STATEMENT_TIMEOUT_MS`, `EDB_LOCK_TIMEOUT_MS`,
/// `EDB_TCP_USER_TIMEOUT_MS`, `EDB_KEEPALIVES_IDLE_SECONDS`,
/// `EDB_KEEPALIVES_INTERVAL_SECONDS`, `EDB_KEEPALIVES_RETRIES`.
/// `EDB_KEEPALIVES` accepts only `true` or `false`. Omitted I/O settings
/// preserve the library/driver policy; no universal production deadline is added.
pub fn postgres_config_from_env() -> Result<PostgresConnectionConfig, SemanticError> {
    config_from(|key| match std::env::var(key) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(invalid("config/environment", key)),
    })
}

fn invalid(code: &'static str, key: &'static str) -> SemanticError {
    SemanticError::incorrect(code, format!("missing or invalid {key}"))
}

fn config_from(
    lookup: impl Fn(&'static str) -> Result<Option<String>, SemanticError>,
) -> Result<PostgresConnectionConfig, SemanticError> {
    let required = |key| {
        lookup(key)?
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid("config/environment", key))
    };
    let parameters = required("EDB_POSTGRES_URL")?;
    if lookup("EDB_POSTGRES_TRANSPORT")?.is_some() {
        return Err(invalid(
            "config/use-postgres-sslmode",
            "EDB_POSTGRES_TRANSPORT",
        ));
    }
    let mut config = PostgresConnectionConfig::parse(parameters)?;
    if let Some(path) = lookup("EDB_POSTGRES_TLS_ROOT")? {
        if !config.tls_required() {
            return Err(invalid(
                "config/tls-root-requires-tls",
                "EDB_POSTGRES_TLS_ROOT",
            ));
        }
        let bytes = std::fs::read(path)
            .map_err(|_| invalid("config/tls-root-read", "EDB_POSTGRES_TLS_ROOT"))?;
        config = config.with_root_certificate_pem(bytes)?;
    }
    let number = |key| -> Result<Option<u64>, SemanticError> {
        lookup(key)?
            .map(|value| {
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| invalid("config/positive-integer", key))
            })
            .transpose()
    };
    let keepalives = lookup("EDB_KEEPALIVES")?
        .map(|value| match value.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(invalid("config/boolean", "EDB_KEEPALIVES")),
        })
        .transpose()?;
    config = config.with_io_policy(PostgresIoPolicy {
        connect_timeout: number("EDB_CONNECT_TIMEOUT_MS")?.map(Duration::from_millis),
        statement_timeout: number("EDB_STATEMENT_TIMEOUT_MS")?.map(Duration::from_millis),
        lock_timeout: number("EDB_LOCK_TIMEOUT_MS")?.map(Duration::from_millis),
        tcp_user_timeout: number("EDB_TCP_USER_TIMEOUT_MS")?.map(Duration::from_millis),
        keepalives,
        keepalives_idle: number("EDB_KEEPALIVES_IDLE_SECONDS")?.map(Duration::from_secs),
        keepalives_interval: number("EDB_KEEPALIVES_INTERVAL_SECONDS")?.map(Duration::from_secs),
        keepalives_retries: number("EDB_KEEPALIVES_RETRIES")?
            .map(|value| {
                u32::try_from(value)
                    .map_err(|_| invalid("config/positive-integer", "EDB_KEEPALIVES_RETRIES"))
            })
            .transpose()?,
    })?;
    let ssd_directory = lookup("EDB_SSD_CACHE_DIR")?;
    let ssd_entries = number("EDB_SSD_CACHE_ENTRIES")?;
    let ssd_bytes = number("EDB_SSD_CACHE_BYTES")?;
    if let Some(directory) = ssd_directory {
        if directory.is_empty() {
            return Err(invalid("config/environment", "EDB_SSD_CACHE_DIR"));
        }
        let defaults = crate::SsdCacheLimits::default();
        let max_entries = ssd_entries
            .map(usize::try_from)
            .transpose()
            .map_err(|_| invalid("config/positive-integer", "EDB_SSD_CACHE_ENTRIES"))?
            .unwrap_or(defaults.max_entries);
        config = config.with_ssd_cache(crate::SsdCacheConfig {
            directory: directory.into(),
            limits: crate::SsdCacheLimits {
                max_entries,
                max_bytes: ssd_bytes.unwrap_or(defaults.max_bytes),
            },
        });
    } else if ssd_entries.is_some() || ssd_bytes.is_some() {
        return Err(invalid(
            "config/ssd-directory-required",
            "EDB_SSD_CACHE_DIR",
        ));
    }
    Ok(config)
}
