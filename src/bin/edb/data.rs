//! Text data commands use the ordinary peer and fenced transactor interfaces.
use edb_core::edn::{EdnValue, read_edn, write_edn};
use edb_core::edn_pull::{entity_identifier_from_edn, parse_pull_edn};
use edb_core::edn_query::{EdnQueryArgument, EdnQueryInput, parse_query_edn};
use edb_core::edn_value::{edn_keyword, query_value_to_edn, transaction_report_to_edn};
use edb_core::{
    BackupConnection, Connection, DatabaseValue, Datom, EntityRef, ErrorCategory, PullControl, QueryControl,
    QuerySourceValue, SemanticError, TransactionRequest, postgres_config_from_env,
    remote_client_config_from_env,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
pub const HELP: &str = "
EDN data commands (file path '-' reads stdin; results are EDN on stdout):
  edb transact --database ID --file PATH --request-key KEY --source EDN
    (--endpoint PATH | --remote) [--basis N] [--tx-instant MILLIS] [--timeout-ms N]
  edb with (--database ID | --repository PATH) --file PATH --source EDN [--tx-instant MILLIS]
    [--default-partition KEYWORD]
  edb query [--database ID | --repository PATH] --file PATH [--inputs PATH] [--sources PATH]
    [--as-of N] [--since N] [--history] [--timeout-ms N] [--max-work N]
    [--max-results N] [--max-join-bytes N] [--max-value-bytes N]
    [--query-stats] [--io-context :app/operation]
  edb pull (--database ID | --repository PATH) --file PATH --entity EDN
    [--as-of N] [--since N] [--max-depth N] [--max-entities N]
    [--io-context :app/operation]

transact requires a stable caller-supplied request key; reuse it AND the original
intent/options after an unknown outcome. --basis is an optional optimistic guard.
--source is an entity reference for the source of every fact in the transaction.
with is a local preview and never commits. Its temporary IDs are not reserved.
Use the same --default-partition as the transactor to preview default placement.
Explicit partition hints override the default; existing entities never move.
query --inputs is a vector of non-source :in arguments (including rules/patterns).
--sources is a map from source symbols to tuple rows or descriptors such as
{$past {:database \"customers\" :as-of 42} $log {:database \"customers\" :log true}}.
The default $ source is the selected --database value. Reads never start a writer.
--repository reads a local backup without PostgreSQL configuration. It defaults
to the latest point; select an exact point with BOTH --backup-basis N and
--backup-generation N. Named sources also accept {:repository PATH :basis N
:generation N :log true}; omit :log for a database value.
Data-only queries need no --database or PostgreSQL connection configuration.
The same PostgreSQL/TLS credential configuration applies. --remote uses verified
TLS writer discovery and EDB_REMOTE_TOKEN_FILE, not plaintext TCP.
Input files are limited to16MiB each. Output conversion/printing is bounded;
query defaults to a30s evaluation timeout. See docs/01_tutorials/00_edn_workflow.md for format/type limits.
";

struct Arguments {
    command: String,
    values: BTreeMap<String, String>,
    switches: BTreeSet<String>,
}
pub fn dispatch(raw: &[String]) -> Option<Result<(), SemanticError>> {
    let command = raw.first()?.as_str();
    if !matches!(command, "transact" | "with" | "query" | "pull") {
        return None;
    }
    Some(parse(command, &raw[1..]).and_then(run))
}
fn usage(message: &str) -> SemanticError {
    SemanticError::incorrect("cli/edn-usage", message)
}
fn parse(command: &str, raw: &[String]) -> Result<Arguments, SemanticError> {
    let (flags, switches): (&[&str], &[&str]) = match command {
        "transact" => (
            &[
                "--database",
                "--file",
                "--request-key",
                "--source",
                "--endpoint",
                "--basis",
                "--tx-instant",
                "--timeout-ms",
            ],
            &["--remote"],
        ),
        "with" => (
            &[
                "--database",
                "--repository",
                "--backup-basis",
                "--backup-generation",
                "--file",
                "--tx-instant",
                "--default-partition",
                "--source",
            ],
            &[],
        ),
        "query" => (
            &[
                "--database",
                "--repository",
                "--backup-basis",
                "--backup-generation",
                "--file",
                "--inputs",
                "--sources",
                "--as-of",
                "--since",
                "--timeout-ms",
                "--max-work",
                "--max-results",
                "--max-join-bytes",
                "--max-value-bytes",
                "--io-context",
            ],
            &["--history", "--query-stats"],
        ),
        "pull" => (
            &[
                "--database",
                "--repository",
                "--backup-basis",
                "--backup-generation",
                "--file",
                "--entity",
                "--as-of",
                "--since",
                "--max-depth",
                "--max-entities",
                "--io-context",
            ],
            &[],
        ),
        _ => unreachable!(),
    };
    let mut args = Arguments {
        command: command.into(),
        values: BTreeMap::new(),
        switches: BTreeSet::new(),
    };
    let mut rest = raw.iter();
    while let Some(flag) = rest.next() {
        if switches.contains(&flag.as_str()) {
            if !args.switches.insert(flag.clone()) {
                return Err(usage("duplicate option"));
            }
        } else if flags.contains(&flag.as_str()) {
            let value = rest
                .next()
                .filter(|v| !v.is_empty() && !v.starts_with("--"))
                .ok_or_else(|| usage("option requires a value"))?;
            if args.values.insert(flag.clone(), value.clone()).is_some() {
                return Err(usage("duplicate option"));
            }
        } else {
            return Err(usage("unknown or misplaced option; run edb --help"));
        }
    }
    if command == "transact" {
        args.required("--database")?;
    } else {
        let database = args.values.contains_key("--database");
        let repository = args.values.contains_key("--repository");
        if database && repository || command != "query" && !database && !repository {
            return Err(usage("select exactly one of --database or --repository"));
        }
        let basis = args.values.contains_key("--backup-basis");
        let generation = args.values.contains_key("--backup-generation");
        if basis != generation || (basis && !repository) {
            return Err(usage(
                "backup point requires --repository and both --backup-basis and --backup-generation",
            ));
        }
    }
    args.required("--file")?;
    if matches!(command, "transact" | "with") {
        EntityRef::from_edn(args.required("--source")?)?;
    }
    diagnostic_context(&args)?;
    super::transaction_defaults(args.values.get("--default-partition").map(String::as_str))?;
    if command == "transact" {
        args.required("--request-key")?;
        if args.values.contains_key("--endpoint") == args.switches.contains("--remote") {
            return Err(usage(
                "transact requires exactly one of --endpoint or --remote",
            ));
        }
    }
    if command == "pull" {
        args.required("--entity")?;
    }
    let stdin_count = ["--file", "--inputs", "--sources"]
        .iter()
        .filter(|flag| args.values.get(**flag).is_some_and(|v| v == "-"))
        .count();
    if stdin_count > 1 {
        return Err(usage("only one input may read stdin"));
    }
    for flag in [
        "--basis",
        "--backup-basis",
        "--backup-generation",
        "--as-of",
        "--since",
        "--timeout-ms",
        "--max-work",
        "--max-results",
        "--max-join-bytes",
        "--max-value-bytes",
        "--max-depth",
        "--max-entities",
    ] {
        if args.values.contains_key(flag) {
            args.number(flag, 0)?;
        }
    }
    if args
        .values
        .get("--tx-instant")
        .is_some_and(|v| v.parse::<i64>().is_err())
    {
        return Err(usage("--tx-instant requires signed 64-bit milliseconds"));
    }
    if args.values.contains_key("--timeout-ms") && args.number("--timeout-ms", 1)? == 0 {
        return Err(usage("timeout must be positive"));
    }
    Ok(args)
}
impl Arguments {
    fn required(&self, flag: &str) -> Result<&str, SemanticError> {
        self.values
            .get(flag)
            .map(String::as_str)
            .ok_or_else(|| usage("required data command option is missing; run edb --help"))
    }
    fn number(&self, flag: &str, default: u64) -> Result<u64, SemanticError> {
        self.values
            .get(flag)
            .map(|v| {
                v.parse()
                    .map_err(|_| usage("numeric option requires a nonnegative 64-bit integer"))
            })
            .unwrap_or(Ok(default))
    }
    fn size(&self, flag: &str, default: usize) -> Result<usize, SemanticError> {
        usize::try_from(self.number(flag, default as u64)?)
            .map_err(|_| usage("numeric option exceeds this platform's supported size"))
    }
    fn preview_instant(&self) -> Result<i64, SemanticError> {
        match self.values.get("--tx-instant") {
            Some(value) => value.parse().map_err(|_| usage("invalid preview instant")),
            None => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|v| i64::try_from(v.as_millis()).ok())
                .ok_or_else(|| usage("preview clock is outside the supported range")),
        }
    }
}
fn read_file(path: &str) -> Result<String, SemanticError> {
    let mut input: Box<dyn Read> = if path == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(std::fs::File::open(path).map_err(|_| usage("could not open EDN input file"))?)
    };
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| usage("could not read EDN input"))?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(usage("EDN input exceeds16MiB"));
    }
    String::from_utf8(bytes).map_err(|_| usage("EDN input must be UTF-8"))
}
fn output(value: &EdnValue) -> Result<(), SemanticError> {
    let text = write_edn(value)?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|_| stdout.write_all(b"\n"))
        .and_then(|_| stdout.flush())
        .map_err(|_| {
            SemanticError::new(
                ErrorCategory::Unavailable,
                "cli/edn-output",
                "could not write EDN result; a failed observation does not imply rollback",
            )
        })
}
enum ReadConnection {
    Live(Connection),
    Backup(Box<BackupConnection>),
}
impl ReadConnection {
    fn db(&self) -> DatabaseValue {
        match self {
            Self::Live(connection) => connection.db(),
            Self::Backup(connection) => connection.db(),
        }
    }
    fn log(&self) -> edb_core::LogValue {
        match self {
            Self::Live(connection) => connection.log(),
            Self::Backup(connection) => connection.log(),
        }
    }
}

fn open_backup(
    repository: &str,
    basis: Option<u64>,
    generation: Option<u64>,
) -> Result<BackupConnection, SemanticError> {
    match (basis, generation) {
        (None, None) => BackupConnection::open(repository),
        (Some(basis), Some(generation)) => {
            let point =
                edb_core::PortableBackup::list_backup_points(std::path::Path::new(repository))?
                    .into_iter()
                    .find(|point| point.basis_t == basis && point.log_generation == generation)
                    .ok_or_else(|| usage("requested backup point does not exist in repository"))?;
            BackupConnection::open_point(repository, &point)
        }
        _ => Err(usage("backup point requires both basis and generation")),
    }
}

fn open_primary(args: &Arguments) -> Result<ReadConnection, SemanticError> {
    if let Some(repository) = args.values.get("--repository") {
        let coordinate = |flag| {
            args.values
                .contains_key(flag)
                .then(|| args.number(flag, 0))
                .transpose()
        };
        Ok(ReadConnection::Backup(Box::new(open_backup(
            repository,
            coordinate("--backup-basis")?,
            coordinate("--backup-generation")?,
        )?)))
    } else {
        Ok(ReadConnection::Live(Connection::connect_configured(
            postgres_config_from_env()?,
            args.required("--database")?,
            128,
        )?))
    }
}

fn run(args: Arguments) -> Result<(), SemanticError> {
    let text = read_file(args.required("--file")?)?;
    // Reject lexical errors before opening any database or attempting a write.
    read_edn(&text)?;
    if args.command == "query" {
        return run_query(&args, &text);
    }
    let diagnostic = diagnostic_context(&args)?;
    let _scope = diagnostic.as_ref().map(edb_core::OperationContext::enter);
    let connection = open_primary(&args)?;
    match args.command.as_str() {
        "transact" => {
            let ReadConnection::Live(connection) = &connection else {
                return Err(usage("transact requires a live database"));
            };
            let config = postgres_config_from_env()?;
            let mut request = TransactionRequest::from_edn(
                args.required("--request-key")?,
                &text,
                EntityRef::from_edn(args.required("--source")?)?,
            )?;
            if args.values.contains_key("--basis") {
                request = request.comparing_basis(args.number("--basis", 0)?);
            }
            if args.values.contains_key("--tx-instant") {
                request = request.with_tx_instant(args.preview_instant()?);
            }
            let timeout = Duration::from_millis(args.number("--timeout-ms", 30_000)?);
            let commit = if args.switches.contains("--remote") {
                let endpoint = connection.discover_remote_writer(&config)?;
                connection.transact_remote(
                    &endpoint,
                    &remote_client_config_from_env()?,
                    request,
                    timeout,
                )?
            } else {
                connection.transact_socket(args.required("--endpoint")?, request, timeout)?
            };
            // A confirmed commit remains confirmed even if opening its local
            // report or encoding its payload fails. Always retain a minimal receipt.
            let mut fields = vec![
                (key("committed"), EdnValue::Bool(true)),
                (key("basis-t"), integer(commit.basis_t)),
                (key("replayed"), EdnValue::Bool(commit.replayed)),
                (
                    key("tx-hash"),
                    EdnValue::String(commit.tx_hash.iter().map(|b| format!("{b:02x}")).collect()),
                ),
            ];
            match commit.report {
                Ok(report) => match report_fields(
                    report.db_before.basis_t(),
                    report.db_after.basis_t(),
                    &report.tx_data,
                    &report.tempids,
                ) {
                    Ok(report) => fields.extend(report),
                    Err(error) => {
                        fields.push((key("report-error"), EdnValue::String(error.code.into())))
                    }
                },
                Err(error) => {
                    fields.push((key("report-error"), EdnValue::String(error.code.into())))
                }
            }
            if let Err(error) = output(&EdnValue::Map(fields)) {
                if error.code == "cli/edn-output" {
                    return Err(error);
                }
                // The short receipt may still be printable if only the full
                // report exceeded output admission. Never describe this as rejection.
                output(&EdnValue::Map(vec![
                    (key("committed"), EdnValue::Bool(true)),
                    (key("basis-t"), integer(commit.basis_t)),
                    (key("replayed"), EdnValue::Bool(commit.replayed)),
                    (
                        key("tx-hash"),
                        EdnValue::String(
                            commit.tx_hash.iter().map(|b| format!("{b:02x}")).collect(),
                        ),
                    ),
                    (key("report-error"), EdnValue::String(error.code.into())),
                ]))?;
            }
        }
        "with" => {
            let db = connection.db();
            let defaults = super::transaction_defaults(
                args.values.get("--default-partition").map(String::as_str),
            )?;
            let report = db.with_edn_with_defaults(
                &text,
                args.preview_instant()?,
                EntityRef::from_edn(args.required("--source")?)?,
                &defaults,
            )?;
            let mut fields = report_fields(
                report.db_before.basis_t(),
                report.db_after.basis_t(),
                &report.tx_data,
                &report.tempids,
            )?;
            fields.push((key("committed"), EdnValue::Bool(false)));
            fields.push((key("speculative"), EdnValue::Bool(true)));
            output(&EdnValue::Map(fields))?;
        }
        "pull" => {
            let pattern = parse_pull_edn(&text)?;
            let identifier = entity_identifier_from_edn(&read_edn(args.required("--entity")?)?)?;
            let db = view(connection.db(), &args)?;
            let control = PullControl {
                max_depth: args.size("--max-depth", usize::MAX)?,
                max_entities: args.size("--max-entities", usize::MAX)?,
                ..PullControl::default()
            };
            let value = query_value_to_edn(&db.pull_with_control(&pattern, identifier, &control)?)?;
            output(&with_io_report(value, diagnostic.as_ref()))?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn run_query(args: &Arguments, text: &str) -> Result<(), SemanticError> {
    let diagnostic = diagnostic_context(args)?;
    let _scope = diagnostic.as_ref().map(edb_core::OperationContext::enter);
    let query = parse_query_edn(text)?;
    let inputs = args
        .values
        .get("--inputs")
        .map(|path| read_file(path).and_then(|s| read_edn(&s)))
        .transpose()?
        .unwrap_or_else(|| EdnValue::Vector(Vec::new()));
    let EdnValue::Vector(input_values) = &inputs else {
        return Err(usage(
            "--inputs must contain a vector of non-source arguments",
        ));
    };
    let sources = args
        .values
        .get("--sources")
        .map(|path| read_file(path).and_then(|s| read_edn(&s)))
        .transpose()?
        .unwrap_or_else(|| EdnValue::Map(Vec::new()));
    let EdnValue::Map(source_values) = &sources else {
        return Err(usage("--sources must contain a map"));
    };
    let source_values = source_descriptors(source_values)?;
    let mut values = input_values.iter();
    let mut supplied = BTreeSet::new();
    let mut primary = None;
    let mut arguments = Vec::new();
    for input in &query.inputs {
        match input {
            EdnQueryInput::Source(name) => {
                if let Some(descriptor) = source_values.get(name) {
                    supplied.insert(name.clone());
                    if matches!(
                        descriptor,
                        EdnValue::Vector(_) | EdnValue::List(_) | EdnValue::Set(_)
                    ) {
                        // Raw data uses exactly the public EDN relation adapter.
                        arguments.push(EdnQueryArgument::Data((*descriptor).clone()));
                    } else {
                        if primary.is_none()
                            && (args.values.contains_key("--database")
                                || args.values.contains_key("--repository"))
                        {
                            primary = Some(open_primary(args)?);
                        }
                        arguments.push(EdnQueryArgument::Source(source_value(
                            descriptor,
                            primary.as_ref(),
                        )?));
                    }
                } else if name == "$" {
                    if primary.is_none() {
                        primary = Some(open_primary(args)?);
                    }
                    arguments.push(EdnQueryArgument::Source(QuerySourceValue::Database(view(
                        primary.as_ref().expect("opened primary").db(),
                        args,
                    )?)));
                } else {
                    return Err(usage("a named query source has no --sources descriptor"));
                }
            }
            _ => arguments.push(EdnQueryArgument::Data(
                values
                    .next()
                    .ok_or_else(|| usage("not enough non-source query inputs"))?
                    .clone(),
            )),
        }
    }
    if values.next().is_some() {
        return Err(usage("too many non-source query inputs"));
    }
    if supplied.len() != source_values.len() {
        return Err(usage("unused or invalid query source descriptor"));
    }
    let bound = query.bind(&arguments)?;
    let control = QueryControl {
        diagnostics: args
            .switches
            .contains("--query-stats")
            .then(edb_core::QueryDiagnosticOptions::default),
        timeout: Some(Duration::from_millis(args.number("--timeout-ms", 30_000)?)),
        max_work: args.size("--max-work", usize::MAX)?,
        max_result_rows: args.size("--max-results", usize::MAX)?,
        max_join_bytes: args.size("--max-join-bytes", 4 * 1024 * 1024)?,
        max_value_bytes: args.size("--max-value-bytes", 16 * 1024 * 1024)?,
        ..QueryControl::default()
    };
    let result = bound.execute(&control, None)?;
    let mut value = bound.result_to_edn(&result.result)?;
    if let Some(stats) = &result.diagnostics {
        value = EdnValue::Map(vec![
            (key("ret"), value),
            (
                key("query-stats"),
                edb_core::query_diagnostics_to_edn(stats),
            ),
        ]);
        if let (Some(context), EdnValue::Map(fields)) = (&diagnostic, &mut value) {
            fields.push((
                key("io-stats"),
                edb_core::io_report_to_edn(&context.report()),
            ));
        }
    } else {
        value = with_io_report(value, diagnostic.as_ref());
    }
    output(&value)
}

fn diagnostic_context(
    args: &Arguments,
) -> Result<Option<edb_core::OperationContext>, SemanticError> {
    args.values
        .get("--io-context")
        .map(|text| {
            let EdnValue::Keyword(name) = read_edn(text)? else {
                return Err(usage("--io-context requires a qualified keyword"));
            };
            edb_core::OperationContext::named(edb_core::OperationKind::Application, name)
        })
        .transpose()
}

fn with_io_report(value: EdnValue, context: Option<&edb_core::OperationContext>) -> EdnValue {
    match context {
        None => value,
        Some(context) => EdnValue::Map(vec![
            (key("ret"), value),
            (
                key("io-stats"),
                edb_core::io_report_to_edn(&context.report()),
            ),
        ]),
    }
}
fn key(name: &str) -> EdnValue {
    edn_keyword("edb", name)
}
fn integer(value: u64) -> EdnValue {
    i64::try_from(value)
        .map(EdnValue::Long)
        .unwrap_or_else(|_| EdnValue::BigInt(value.into()))
}
fn report_fields(
    before: u64,
    after: u64,
    datoms: &[Datom],
    tempids: &BTreeMap<String, u64>,
) -> Result<Vec<(EdnValue, EdnValue)>, SemanticError> {
    let EdnValue::Map(fields) = transaction_report_to_edn(before, after, datoms, tempids)? else {
        unreachable!("report encoder produces a map")
    };
    Ok(fields)
}
fn view(mut db: DatabaseValue, args: &Arguments) -> Result<DatabaseValue, SemanticError> {
    if args.values.contains_key("--as-of") {
        db = db.as_of(args.number("--as-of", 0)?);
    }
    if args.values.contains_key("--since") {
        db = db.since(args.number("--since", 0)?);
    }
    if args.switches.contains("--history") {
        db = db.history();
    }
    Ok(db)
}
fn source_name(value: &EdnValue) -> Option<String> {
    match value {
        EdnValue::Symbol(s) => Some(s.qualified_name()),
        EdnValue::String(s) => Some(s.clone()),
        _ => None,
    }
}
fn source_descriptors(
    fields: &[(EdnValue, EdnValue)],
) -> Result<BTreeMap<String, &EdnValue>, SemanticError> {
    let mut sources = BTreeMap::new();
    for (key, value) in fields {
        let name = source_name(key)
            .filter(|name| name.starts_with('$'))
            .ok_or_else(|| usage("query source names must be source symbols or strings"))?;
        if sources.insert(name, value).is_some() {
            return Err(usage("query source names collide after normalization"));
        }
    }
    Ok(sources)
}
fn source_value(
    value: &EdnValue,
    primary: Option<&ReadConnection>,
) -> Result<QuerySourceValue, SemanticError> {
    let EdnValue::Map(fields) = value else {
        return Err(usage("source must be rows or a database descriptor"));
    };
    let mut options = BTreeMap::new();
    for (key, value) in fields {
        let EdnValue::Keyword(key) = key else {
            return Err(usage("source descriptor keys must be keywords"));
        };
        if key.namespace.is_some()
            || !matches!(
                key.name.as_str(),
                "database"
                    | "repository"
                    | "basis"
                    | "generation"
                    | "as-of"
                    | "since"
                    | "history"
                    | "log"
            )
        {
            return Err(usage("unknown source descriptor field"));
        }
        options.insert(key.name.as_str(), value);
    }
    if options.contains_key("database") && options.contains_key("repository") {
        return Err(usage("source cannot select both :database and :repository"));
    }
    if (options.contains_key("basis") || options.contains_key("generation"))
        && !options.contains_key("repository")
    {
        return Err(usage(":basis and :generation select a :repository point"));
    }
    let coordinate = |name| -> Result<Option<u64>, SemanticError> {
        options
            .get(name)
            .map(|value| match value {
                EdnValue::Long(t) => {
                    u64::try_from(*t).map_err(|_| usage("backup coordinate must be nonnegative"))
                }
                _ => Err(usage("backup coordinate must be a nonnegative integer")),
            })
            .transpose()
    };
    let opened;
    let connection = match (options.get("repository"), options.get("database")) {
        (Some(EdnValue::String(repository)), None) => {
            opened = ReadConnection::Backup(Box::new(open_backup(
                repository,
                coordinate("basis")?,
                coordinate("generation")?,
            )?));
            &opened
        }
        (Some(_), _) => return Err(usage("repository path must be a string")),
        (None, Some(EdnValue::String(name))) => {
            let config = postgres_config_from_env()?;
            // A name may have been reused while the primary still holds its
            // old identity. Compare resolved identities, never name spelling
            // against a captured storage ID.
            let selected = edb_core::DatabaseCatalog::connect_configured(&config)?.resolve(name)?;
            if let Some(primary) = primary.filter(|primary| {
                matches!(primary, ReadConnection::Live(connection)
                    if connection.identity().database_id() == selected.database_id
                    && connection.identity().lineage_id() == selected.lineage_id)
            }) {
                primary
            } else {
                let connection = Connection::connect_configured(config, name.as_str(), 128)?;
                if connection.identity().database_id() != selected.database_id
                    || connection.identity().lineage_id() != selected.lineage_id
                {
                    return Err(SemanticError::conflict(
                        "cli/source-identity-changed",
                        "source name changed identity while opening the query source",
                    ));
                }
                opened = ReadConnection::Live(connection);
                &opened
            }
        }
        (None, None) => primary.ok_or_else(|| {
            usage("source requires :database, :repository or a primary selection")
        })?,
        _ => return Err(usage("database source name must be a string")),
    };
    for name in ["history", "log"] {
        if options
            .get(name)
            .is_some_and(|value| !matches!(value, EdnValue::Bool(_)))
        {
            return Err(usage("history and log source flags must be booleans"));
        }
    }
    if matches!(options.get("log"), Some(EdnValue::Bool(true))) {
        if ["as-of", "since", "history"]
            .iter()
            .any(|key| options.contains_key(key))
        {
            return Err(usage("log source does not accept database-view options"));
        }
        return Ok(QuerySourceValue::Log(connection.log()));
    }
    let mut db = connection.db();
    for name in ["as-of", "since"] {
        if let Some(value) = options.get(name) {
            let EdnValue::Long(t) = value else {
                return Err(usage("source basis must be a nonnegative integer"));
            };
            let t = u64::try_from(*t).map_err(|_| usage("source basis must be nonnegative"))?;
            db = if name == "as-of" {
                db.as_of(t)
            } else {
                db.since(t)
            };
        }
    }
    if matches!(options.get("history"), Some(EdnValue::Bool(true))) {
        db = db.history();
    }
    Ok(QuerySourceValue::Database(db))
}
