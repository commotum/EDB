//! Strict argument parsing before any database I/O.
use atomic_core::{SemanticError, TransactionDefaults};
use std::collections::BTreeMap;

pub(super) const HELP: &str = "Atomic — native Rust/PostgreSQL database

Usage:
  atomic install [--writer-role ROLE --peer-role ROLE]
  atomic create --database NAME
  atomic status --database NAME
  atomic consolidate --database ID
  atomic transactor --database ID --endpoint PATH [OPTIONS]
  atomic transactor --database ID --listen IP:PORT --advertise IP:PORT
    --tls-server-name NAME [OPTIONS]

Required environment for live PostgreSQL commands:
  Offline repository commands and data-only queries do not require PostgreSQL.
  ATOMIC_POSTGRES_URL          PostgreSQL connection string; never printed
                              TCP uses verified TLS; sslmode=disable opts out
Optional: ATOMIC_POSTGRES_TLS_ROOT (PEM); ATOMIC_CONNECT_TIMEOUT_MS,
  ATOMIC_STATEMENT_TIMEOUT_MS, ATOMIC_LOCK_TIMEOUT_MS, ATOMIC_TCP_USER_TIMEOUT_MS,
  ATOMIC_KEEPALIVES=true|false, ATOMIC_KEEPALIVES_IDLE_SECONDS,
  ATOMIC_KEEPALIVES_INTERVAL_SECONDS, ATOMIC_KEEPALIVES_RETRIES.
  ATOMIC_SSD_CACHE_DIR opts into a private disposable cache; optional positive
  ATOMIC_SSD_CACHE_ENTRIES and ATOMIC_SSD_CACHE_BYTES bound its shared directory.
Remote TLS: ATOMIC_REMOTE_TLS_CERT (PEM chain), ATOMIC_REMOTE_TLS_KEY (private
  PKCS8 PEM), ATOMIC_REMOTE_TOKEN_FILE (private file containing64 hex digits).
Clients use the same token file and optional ATOMIC_REMOTE_TLS_ROOT PEM trust
  anchor; certificate/name verification is mandatory. No plaintext TCP mode.

Transactor options (numeric limits are positive integers unless stated otherwise):
  --mode active|auto           active fails on contention (default); auto waits as standby
  --standby-poll-ms N          contender interval, default250; stop wakes idle polling
  --health-listen IP:PORT      opt-in plaintext status only: /health and /ready
  --telemetry-ms N             opt-in bounded JSON operational events on stdout
  --holder ID                  lease holder label (default: process-specific)
  --default-partition KEYWORD   default placement for new ordinary entities
                               (default: :db.part/user; install named partitions first)
  --lease-ms N                 default 5000
  --renew-ms N                 default 1000; must be less than lease
  --queue-capacity N           default 64
  --max-in-flight N            default 4
  --request-timeout-ms N       default 30000 (not a commit/rollback guarantee)
  --max-frame-bytes N          default library frame limit
  --index-threshold-bytes N    default library indexing threshold
  --index-max-bytes N          default library recent-memory maximum
  --index-workers N            1..8 independent edit-preparation lanes, default1
  --hint-workers N             0..8 transaction-hint lanes, default1;0 disables
  --tree-cache-entries N       default library writer cache entries
  --tree-cache-bytes N         default library writer cache bytes
  --excision-max-bytes N       eager maintenance allocation account, default512MiB (not RSS)
  --excision-log-batch N       1..4096 replay transactions per step, default256

Use a pre-existing private owner-only endpoint directory (mkdir -m 700).
SIGINT/SIGTERM stop admission, drain bounded local requests, then stop the writer.
Runtime never installs storage or creates databases. Run install/create separately
with administrative credentials. Runtime grants are additive; use pre-existing
dedicated roles.
consolidate is explicit missing-index recovery or
maintenance, not automatic startup. status is catalog state, not deep integrity
verification or proof of a live transactor. See docs/01_tutorials/01_application_workflow.md.
";

pub(super) struct Arguments {
    pub(super) command: String,
    pub(super) options: BTreeMap<String, String>,
}

impl Arguments {
    pub(super) fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, SemanticError> {
        let mut args = args.into_iter();
        let command = args.next().unwrap_or_else(|| "--help".into());
        let allowed: &[&str] = match command.as_str() {
            "--help" | "help" | "--version" => &[],
            "install" => &["--writer-role", "--peer-role"],
            "create" | "status" | "consolidate" => &["--database"],
            "transactor" => &[
                "--mode",
                "--standby-poll-ms",
                "--health-listen",
                "--telemetry-ms",
                "--database",
                "--endpoint",
                "--listen",
                "--advertise",
                "--tls-server-name",
                "--holder",
                "--default-partition",
                "--lease-ms",
                "--renew-ms",
                "--queue-capacity",
                "--max-in-flight",
                "--request-timeout-ms",
                "--max-frame-bytes",
                "--index-threshold-bytes",
                "--index-max-bytes",
                "--index-workers",
                "--hint-workers",
                "--tree-cache-entries",
                "--tree-cache-bytes",
                "--excision-max-bytes",
                "--excision-log-batch",
            ],
            _ => return Err(usage("unknown command; run atomic --help")),
        };
        let mut options = BTreeMap::new();
        while let Some(flag) = args.next() {
            if !allowed.contains(&flag.as_str()) {
                return Err(usage("unknown or misplaced option; run atomic --help"));
            }
            let value = args
                .next()
                .filter(|s| !s.is_empty() && !s.starts_with("--"))
                .ok_or_else(|| usage("option requires a nonempty value"))?;
            if options.insert(flag, value).is_some() {
                return Err(usage("duplicate option"));
            }
        }
        let parsed = Self { command, options };
        match parsed.command.as_str() {
            "create" | "status" | "consolidate" | "transactor" => {
                parsed.required("--database")?;
            }
            "install" => {
                if parsed.options.contains_key("--writer-role")
                    != parsed.options.contains_key("--peer-role")
                {
                    return Err(usage(
                        "--writer-role and --peer-role must be supplied together",
                    ));
                }
                if parsed.options.contains_key("--writer-role")
                    && parsed.options.get("--writer-role") == parsed.options.get("--peer-role")
                {
                    return Err(usage("writer and peer roles must be distinct"));
                }
            }
            _ => {}
        }
        if parsed.command == "transactor" {
            if parsed
                .options
                .get("--mode")
                .is_some_and(|mode| !matches!(mode.as_str(), "active" | "auto"))
            {
                return Err(usage("--mode must be active or auto"));
            }
            if let Some(address) = parsed.options.get("--health-listen") {
                address
                    .parse::<std::net::SocketAddr>()
                    .map_err(|_| usage("--health-listen requires a numeric IP:port"))?;
            }
            if parsed.number("--excision-log-batch", 256)? > 4096 {
                return Err(usage("--excision-log-batch must be 1..4096"));
            }
            transaction_defaults(
                parsed
                    .options
                    .get("--default-partition")
                    .map(String::as_str),
            )?;
            let local = parsed.options.contains_key("--endpoint");
            let remote = parsed.options.contains_key("--listen");
            if local == remote {
                return Err(usage("choose exactly one of --endpoint or --listen"));
            }
            if remote {
                parsed
                    .required("--advertise")?
                    .parse::<std::net::SocketAddr>()
                    .map_err(|_| usage("--advertise requires a numeric IP:port"))?;
                parsed
                    .required("--listen")?
                    .parse::<std::net::SocketAddr>()
                    .map_err(|_| usage("--listen requires a numeric IP:port"))?;
                parsed.required("--tls-server-name")?;
            } else if parsed.options.contains_key("--advertise")
                || parsed.options.contains_key("--tls-server-name")
            {
                return Err(usage("remote address/name options require --listen"));
            }
            // Validate every numeric setting before touching the database.
            for flag in parsed.options.keys().filter(|key| {
                !matches!(
                    key.as_str(),
                    "--database"
                        | "--mode"
                        | "--health-listen"
                        | "--endpoint"
                        | "--holder"
                        | "--default-partition"
                        | "--listen"
                        | "--advertise"
                        | "--tls-server-name"
                )
            }) {
                parsed.number(flag, 1)?;
            }
        }
        Ok(parsed)
    }

    pub(super) fn required(&self, flag: &str) -> Result<&str, SemanticError> {
        self.options
            .get(flag)
            .map(String::as_str)
            .ok_or_else(|| usage(format!("required option {flag} is missing")))
    }

    pub(super) fn number(&self, flag: &str, default: u64) -> Result<u64, SemanticError> {
        self.options
            .get(flag)
            .map(|value| {
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0 || flag == "--hint-workers")
                    .ok_or_else(|| {
                        let kind = if flag == "--hint-workers" {
                            "nonnegative"
                        } else {
                            "positive"
                        };
                        usage(format!("{flag} requires a {kind} integer"))
                    })
            })
            .unwrap_or(Ok(default))
    }

    pub(super) fn size(&self, flag: &str, default: usize) -> Result<usize, SemanticError> {
        usize::try_from(self.number(flag, default as u64)?)
            .map_err(|_| usage(format!("{flag} exceeds this platform's supported size")))
    }
}

pub(super) fn usage(message: impl Into<String>) -> SemanticError {
    SemanticError::incorrect("cli/usage", message)
}

pub(super) fn transaction_defaults(
    partition: Option<&str>,
) -> Result<TransactionDefaults, SemanticError> {
    let Some(text) = partition else {
        return Ok(TransactionDefaults::default());
    };
    let atomic_core::edn::EdnValue::Keyword(name) = atomic_core::edn::read_edn(text)
        .map_err(|_| usage("--default-partition requires one EDN keyword"))?
    else {
        return Err(usage("--default-partition requires one EDN keyword"));
    };
    Ok(TransactionDefaults::default().with_default_partition(name))
}
