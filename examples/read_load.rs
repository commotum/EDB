//! Bounded independent-process read campaign; see the observability guide.
use edb_core::sql_io::SqlClient;
use edb_core::{
    Attribute, BackgroundIndexingConfig, CapacityLimits, Cardinality, Clause, Connection,
    DataPattern, DatabaseValue, EntityRef, FindElement, FindSpec, Keyword, OperationContext,
    OperationKind, PostgresConnectionConfig, PostgresOperator, Query, QueryControl, QueryResult,
    QueryValue, Schema, SqlIoStats, Term, TransactionRequest, TransactionService,
    TransactionServiceConfig, TxOp, Unique, Value, ValueType, Variable, postgres_config_from_env,
    process_sql_stats,
};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const KEY: u32 = 1000;
const BALANCE: u32 = 1001;
const PAYLOAD: u32 = 1002;
const CACHE_ENTRIES: usize = 32;
const DEFAULT_CACHE_BYTES: usize = 1024 * 1024;
const MAX_SAMPLES: usize = 200_000;
const WAIT: Duration = Duration::from_secs(180);

fn cache_bytes() -> Result<usize> {
    let bytes = std::env::var("EDB_READ_CACHE_BYTES")
        .map(|value| value.parse())
        .unwrap_or(Ok(DEFAULT_CACHE_BYTES))?;
    if !(64 * 1024..=16 * 1024 * 1024).contains(&bytes) {
        return Err("EDB_READ_CACHE_BYTES must be 65536..16777216".into());
    }
    Ok(bytes)
}

fn emit(line: impl std::fmt::Display) {
    println!("{line}");
    std::io::stdout().flush().unwrap();
}

#[derive(Default)]
struct Resource {
    cpu_us: u64,
    rss: u64,
    high_water: u64,
}

fn resource() -> Resource {
    let mut result = Resource::default();
    // RUSAGE_SELF includes every thread in this process, not PostgreSQL backends.
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0 {
        let usage = unsafe { usage.assume_init() };
        result.cpu_us = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as u64 * 1_000_000
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as u64;
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            let mut fields = line.split_whitespace();
            let name = fields.next().unwrap_or_default();
            let bytes = fields
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
                * 1024;
            match name {
                "VmRSS:" => result.rss = bytes,
                "VmHWM:" => result.high_water = bytes,
                _ => {}
            }
        }
    }
    result
}

struct Window {
    sql: SqlIoStats,
    resources: Resource,
    start: Instant,
}

impl Window {
    fn new() -> Self {
        Self {
            sql: process_sql_stats(),
            resources: resource(),
            start: Instant::now(),
        }
    }

    fn finish(&self, label: &str, samples: &[u64], foreground: &SqlIoStats, extra: &str) {
        let elapsed = self.start.elapsed().as_micros() as u64;
        let sql = process_sql_stats();
        let resources = resource();
        let operations = sql
            .by_operation
            .iter()
            .map(|(kind, count)| {
                let old = self.sql.by_operation.get(kind).cloned().unwrap_or_default();
                format!(
                    "{kind:?}:{}:{}:{}",
                    count.calls - old.calls,
                    count.errors - old.errors,
                    count.elapsed_nanos.saturating_sub(old.elapsed_nanos) / 1000
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        emit(format!(
            "METRIC label={label} pid={} ops={} wall_us={elapsed} cpu_us={} rss={} hwm={} sql={} connects={} controls={} errors={} sql_us={} stream_us={} result_bytes={} foreground_sql={} foreground_errors={} foreground_result_bytes={} attribution={operations} {extra}",
            std::process::id(),
            samples.len(),
            resources.cpu_us - self.resources.cpu_us,
            resources.rss,
            resources.high_water,
            sql.sql_calls - self.sql.sql_calls,
            sql.connect_calls - self.sql.connect_calls,
            sql.control_calls - self.sql.control_calls,
            sql.errors - self.sql.errors,
            sql.elapsed_nanos.saturating_sub(self.sql.elapsed_nanos) / 1000,
            sql.stream_elapsed_nanos
                .saturating_sub(self.sql.stream_elapsed_nanos)
                / 1000,
            sql.result_cell_bytes - self.sql.result_cell_bytes,
            foreground.sql_calls,
            foreground.errors,
            foreground.result_cell_bytes,
        ));
        emit(format!(
            "SAMPLES {}",
            samples
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
}

fn selective_query(key: i64) -> Result<Query> {
    let entity = Variable::new("entity")?;
    let balance = Variable::new("balance")?;
    Ok(Query::new(
        FindSpec::Tuple(vec![
            FindElement::Variable(entity.clone()),
            FindElement::Variable(balance.clone()),
        ]),
        vec![
            Clause::Pattern(Box::new(DataPattern::new(
                Term::Variable(entity.clone()),
                Term::Constant(Value::Long(KEY.into())),
                Term::Constant(Value::Long(key)),
            ))),
            Clause::Pattern(Box::new(DataPattern::new(
                Term::Variable(entity),
                Term::Constant(Value::Long(BALANCE.into())),
                Term::Variable(balance),
            ))),
        ],
    ))
}

fn selective(database: &DatabaseValue, query: &Query) -> Result<()> {
    let result = database.query(
        query,
        &[],
        &QueryControl {
            timeout: Some(Duration::from_secs(10)),
            max_work: 10_000,
            ..QueryControl::default()
        },
    )?;
    match result.result {
        QueryResult::Tuple(Some(values))
            if matches!(values.as_slice(),
            [QueryValue::Scalar(Value::Ref(_)), QueryValue::Scalar(Value::Long(value))] if *value >= 0) =>
            {}
        other => return Err(format!("unexpected selective result: {other:?}").into()),
    }
    Ok(())
}

fn analytics_query() -> Result<Query> {
    let entity = Variable::new("entity")?;
    let payload = Variable::new("payload")?;
    Ok(Query::new(
        FindSpec::Relation(vec![
            FindElement::Variable(entity.clone()),
            FindElement::Variable(payload.clone()),
        ]),
        vec![Clause::Pattern(Box::new(DataPattern::new(
            Term::Variable(entity),
            Term::Constant(Value::Long(PAYLOAD.into())),
            Term::Variable(payload),
        )))],
    ))
}

fn account_payload(key: usize, bytes: usize) -> String {
    let mut result = format!("account-{key:020}-");
    // Repeatable, distinct ASCII payloads, not identical padding that turns
    // this into a best-case compression demonstration.
    let mut state = (key as u64).wrapping_add(0x9e3779b97f4a7c15);
    while result.len() < bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        result.push(
            b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"[(state % 62) as usize]
                as char,
        );
    }
    result
}

fn analytics(
    database: &DatabaseValue,
    query: &Query,
    records: usize,
    payload: usize,
) -> Result<()> {
    let result = database.query(
        query,
        &[],
        &QueryControl {
            timeout: Some(Duration::from_secs(30)),
            max_work: records * 64 + 10_000,
            max_result_rows: records,
            max_intermediate_rows: records + 1,
            ..QueryControl::default()
        },
    )?;
    let QueryResult::Relation(rows) = result.result else {
        return Err("analytics relation missing".into());
    };
    let mut bytes = 0;
    for row in &rows {
        let [
            QueryValue::Scalar(Value::Ref(_)),
            QueryValue::Scalar(Value::String(value)),
        ] = row.as_slice()
        else {
            return Err("analytics row shape changed".into());
        };
        bytes += value.len();
    }
    if rows.len() != records || bytes != records * payload {
        return Err("analytics count or byte checksum changed".into());
    }
    Ok(())
}

fn reader(database: &str, records: usize, payload: usize) -> Result<()> {
    let cache_bytes = cache_bytes()?;
    let lifetime = Window::new();
    emit("BOOT");
    let mut input = std::io::stdin().lock().lines();
    if input.next().transpose()?.as_deref() != Some("open") {
        return Err("expected open barrier".into());
    }
    let cold = Window::new();
    let connection = Connection::connect_configured_with_cache_limits(
        postgres_config_from_env()?,
        database,
        CACHE_ENTRIES,
        cache_bytes,
    )?;
    let queries = (0..4).map(selective_query).collect::<Result<Vec<_>>>()?;
    let cold_context = OperationContext::new(OperationKind::Query);
    {
        let _scope = cold_context.enter();
        selective(&connection.db(), &queries[0])?;
    }
    cold.finish(
        "cold",
        &[cold.start.elapsed().as_micros() as u64],
        &cold_context.snapshot(),
        "",
    );
    // Warm only a four-key working set, not the full dataset or scan path.
    for i in 0..40 {
        selective(&connection.db(), &queries[i % queries.len()])?;
    }
    let scan_query = analytics_query()?;
    emit("READY");
    for command in input {
        let command = command?;
        let fields = command.split_whitespace().collect::<Vec<_>>();
        if fields.first() == Some(&"stop") {
            break;
        }
        let [mode, millis] = fields.as_slice() else {
            return Err("invalid reader command".into());
        };
        let duration = Duration::from_millis(millis.parse()?);
        let window = Window::new();
        let cache_before = connection.cache_stats();
        let context = OperationContext::new(OperationKind::Query);
        let mut samples = Vec::with_capacity(MAX_SAMPLES);
        {
            let _scope = context.enter();
            while window.start.elapsed() < duration && samples.len() < MAX_SAMPLES {
                let start = Instant::now();
                if *mode == "scan" {
                    analytics(&connection.db(), &scan_query, records, payload)?;
                } else {
                    selective(&connection.db(), &queries[samples.len() % queries.len()])?;
                }
                samples.push(start.elapsed().as_micros() as u64);
            }
        }
        let cache = connection.cache_stats();
        if cache.peak_entries > CACHE_ENTRIES || cache.peak_bytes > cache_bytes {
            return Err("native cache invariant failed".into());
        }
        window.finish(mode, &samples, &context.snapshot(), &format!(
            "cache_entries={} cache_bytes={} cache_peak_bytes={} cache_hits={} cache_misses={} cache_evictions={} cache_oversized_bypasses={} sample_cap={} scan_payload_bytes={}",
            cache.current_entries, cache.current_bytes, cache.peak_bytes,
            cache.hits - cache_before.hits, cache.misses - cache_before.misses,
            cache.evictions - cache_before.evictions,
            cache.oversized_bypasses - cache_before.oversized_bypasses, usize::from(samples.len() == MAX_SAMPLES),
            if *mode == "scan" { records * payload } else { 0 },
        ));
    }
    drop(connection);
    lifetime.finish("reader_lifetime", &[], &SqlIoStats::default(), "");
    Ok(())
}

fn writer(database: &str, records: usize, payload: usize) -> Result<()> {
    let lifetime = Window::new();
    let config = postgres_config_from_env()?;
    let service = TransactionService::start_with_indexing(
        TransactionServiceConfig {
            connection: config.clone(),
            database_id: database.to_owned(),
            holder_id: format!("read-load-{}", std::process::id()),
            lease_duration: Duration::from_secs(5),
            renew_interval: Duration::from_millis(100),
            queue_capacity: 16,
            capacity_limits: CapacityLimits {
                writer_tree_cache_entries: 256,
                writer_tree_cache_bytes: 4 * 1024 * 1024,
                ..CapacityLimits::default()
            },
        },
        BackgroundIndexingConfig {
            memory_index_threshold_bytes: 2 * 1024 * 1024,
            memory_index_max_bytes: 8 * 1024 * 1024,
        },
    )?;
    let seed = Window::new();
    for begin in (0..records).step_by(128) {
        let mut ops = Vec::new();
        for key in begin..records.min(begin + 128) {
            let entity = EntityRef::Temp(format!("a{key}"));
            let text = account_payload(key, payload);
            for (attribute, value) in [
                (KEY, Value::Long(key as i64)),
                (BALANCE, Value::Long(key as i64)),
                (PAYLOAD, Value::String(text)),
            ] {
                ops.push(TxOp::Add {
                    entity: entity.clone(),
                    attribute,
                    value: value.into(),
                });
            }
        }
        service
            .client()
            .transact(TransactionRequest::new(format!("seed-{begin}"), ops), WAIT)?;
    }
    // Make the seed a durable indexed base before opening cold peers: recent
    // novelty must not accidentally substitute for the cache-exceeding scan.
    let route = edb_core::DatabaseCatalog::connect_configured(&config)?
        .resolve(database)?
        .database_id;
    PostgresOperator::connect_configured(&config)?.consolidate_database(&route)?;
    seed.finish(
        "seed",
        &[],
        &SqlIoStats::default(),
        &format!("records={records} payload_bytes={}", records * payload),
    );
    emit("READY");
    let mut sequence = 0;
    for command in std::io::stdin().lock().lines() {
        let command = command?;
        let fields = command.split_whitespace().collect::<Vec<_>>();
        if fields.first() == Some(&"stop") {
            break;
        }
        let [mode, millis] = fields.as_slice() else {
            return Err("invalid writer command".into());
        };
        let duration = Duration::from_millis(millis.parse()?);
        let window = Window::new();
        let mut samples = Vec::new();
        while window.start.elapsed() < duration {
            if *mode == "write" {
                sequence += 1;
                let start = Instant::now();
                service.client().transact(
                    TransactionRequest::new(
                        format!("mixed-{sequence}"),
                        vec![TxOp::Add {
                            entity: EntityRef::Lookup {
                                attribute: KEY,
                                value: Value::Long(0),
                            },
                            attribute: BALANCE,
                            value: Value::Long(sequence).into(),
                        }],
                    ),
                    WAIT,
                )?;
                samples.push(start.elapsed().as_micros() as u64);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let cache = service.writer_residency_stats();
        if cache.eager_database_values != 0 {
            return Err("writer became eager".into());
        }
        window.finish(
            mode,
            &samples,
            &SqlIoStats::default(),
            &format!(
                "cache_entries={} cache_bytes={} recent_bytes={}",
                cache.tree_cache_entries, cache.tree_cache_bytes, cache.recent_accounted_bytes
            ),
        );
    }
    service.shutdown();
    lifetime.finish("writer_lifetime", &[], &SqlIoStats::default(), "");
    Ok(())
}

struct Process {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Process {
    fn spawn(
        role: &str,
        url: &str,
        database: &str,
        records: usize,
        payload: usize,
    ) -> Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .args([role, database, &records.to_string(), &payload.to_string()])
            .env("EDB_POSTGRES_URL", url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().unwrap();
        // Only four protocol lines can wait in userspace; sample payloads are
        // separately bounded by MAX_SAMPLES. No unbounded report subscription.
        let (sender, lines) = mpsc::sync_channel(4);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self { child, lines })
    }
    fn line(&self) -> Result<String> {
        Ok(self.lines.recv_timeout(WAIT)?)
    }
    fn command(&mut self, line: &str) -> Result<()> {
        writeln!(self.child.stdin.as_mut().unwrap(), "{line}")?;
        self.child.stdin.as_mut().unwrap().flush()?;
        Ok(())
    }
    fn metric(&self) -> Result<Measurement> {
        Measurement::parse(self.line()?, self.line()?)
    }
    fn stop(mut self) -> Result<Measurement> {
        self.command("stop")?;
        let result = self.metric()?;
        if !self.child.wait()?.success() {
            return Err("child process failed".into());
        }
        Ok(result)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Measurement {
    line: String,
    fields: BTreeMap<String, u64>,
    samples: Vec<u64>,
}

impl Measurement {
    fn parse(line: String, samples: String) -> Result<Self> {
        if !line.starts_with("METRIC ") || !samples.starts_with("SAMPLES ") {
            return Err(format!("invalid metric protocol: {line}").into());
        }
        let fields = line
            .split_whitespace()
            .filter_map(|field| {
                let (key, value) = field.split_once('=')?;
                Some((key.to_owned(), value.parse().ok()?))
            })
            .collect();
        let samples = samples[8..]
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::parse)
            .collect::<std::result::Result<Vec<u64>, _>>()?;
        if samples.len() > MAX_SAMPLES {
            return Err("sample cap exceeded".into());
        }
        Ok(Self {
            line,
            fields,
            samples,
        })
    }
    fn value(&self, name: &str) -> u64 {
        self.fields.get(name).copied().unwrap_or(0)
    }
}

fn percentile(samples: &mut [u64], percent: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    samples[(samples.len() * percent).div_ceil(100).saturating_sub(1)]
}

fn report(label: &str, measurements: &[Measurement], wall: Duration) {
    let mut samples = measurements
        .iter()
        .flat_map(|m| m.samples.iter().copied())
        .collect::<Vec<_>>();
    let sum = |name| measurements.iter().map(|m| m.value(name)).sum::<u64>();
    emit(format!(
        "SUMMARY phase={label} processes={} ops={} wall_ms={} ops_per_second={:.2} p50_us={} p95_us={} p99_us={} sql={} connects={} controls={} errors={} sql_us={} cpu_us={} rss_sum={} hwm_sum={} result_bytes={} foreground_sql={} foreground_errors={}",
        measurements.len(),
        samples.len(),
        wall.as_millis(),
        samples.len() as f64 / wall.as_secs_f64(),
        percentile(&mut samples, 50),
        percentile(&mut samples, 95),
        percentile(&mut samples, 99),
        sum("sql"),
        sum("connects"),
        sum("controls"),
        sum("errors"),
        sum("sql_us"),
        sum("cpu_us"),
        sum("rss"),
        sum("hwm"),
        sum("result_bytes"),
        sum("foreground_sql"),
        sum("foreground_errors"),
    ));
    for measurement in measurements {
        emit(&measurement.line);
    }
}

fn parameter(connection: &str, name: &str, value: &str) -> String {
    if connection.starts_with("postgres://") || connection.starts_with("postgresql://") {
        let encoded = value
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>();
        format!(
            "{connection}{}{name}={encoded}",
            if connection.contains('?') { "&" } else { "?" }
        )
    } else {
        format!(
            "{connection} {name}='{}'",
            value.replace('\\', "\\\\").replace('\'', "\\'")
        )
    }
}

fn scoped_config(url: &str, base: &PostgresConnectionConfig) -> Result<PostgresConnectionConfig> {
    let mut config = if base.tls_required() {
        PostgresConnectionConfig::require_tls(url)
    } else {
        PostgresConnectionConfig::plaintext(url)
    };
    if let Ok(path) = std::env::var("EDB_POSTGRES_TLS_ROOT") {
        config = config.with_root_certificate_pem(std::fs::read(path)?)?;
    }
    Ok(config.with_io_policy(base.io_policy().clone())?)
}

struct Fixture {
    client: SqlClient,
    schema: String,
    writer: String,
    peer: String,
    cleaned: bool,
}
impl Fixture {
    fn cleanup(&mut self) -> Result<()> {
        self.client
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))?;
        self.client.batch_execute(&format!(
            "DROP OWNED BY {}, {}; DROP ROLE {}; DROP ROLE {}",
            self.writer, self.peer, self.writer, self.peer
        ))?;
        self.cleaned = true;
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        // Exact generated schema/roles only; never restart PostgreSQL or global GC.
        let _ = self
            .client
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema));
        let _ = self.client.batch_execute(&format!(
            "DROP OWNED BY {}, {}; DROP ROLE {}; DROP ROLE {}",
            self.writer, self.peer, self.writer, self.peer
        ));
    }
}

fn campaign(records: usize, millis: u64, payload: usize) -> Result<()> {
    let cache_bytes = cache_bytes()?;
    let lifetime = Window::new();
    let config = postgres_config_from_env()?;
    if config.ssd_cache_config().is_some() {
        return Err("this RAM-cache campaign requires SSD cache configuration to be unset".into());
    }
    let url = std::env::var("EDB_POSTGRES_URL")?;
    let unique = format!(
        "read_load_{}_{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let writer_role = format!("{unique}_w");
    let peer_role = format!("{unique}_p");
    let password = format!("{:032x}", rand::random::<u128>());
    let mut admin = SqlClient::connect_with(|| config.connect())?;
    admin.batch_execute(&format!("CREATE SCHEMA {unique}; CREATE ROLE {writer_role} LOGIN PASSWORD '{password}'; CREATE ROLE {peer_role} LOGIN PASSWORD '{password}'"))?;
    let mut fixture = Fixture {
        client: admin,
        schema: unique.clone(),
        writer: writer_role.clone(),
        peer: peer_role.clone(),
        cleaned: false,
    };
    let scoped = parameter(
        &url,
        "options",
        &format!("-csearch_path={unique},pg_catalog"),
    );
    let scoped_config = scoped_config(&scoped, &config)?;
    edb_core::storage::PgBlockStore::install(&scoped_config)?;
    edb_core::storage::PgBlockStore::connect(&scoped_config)?
        .grant_runtime_privileges(&writer_role, &peer_role)?;
    let mut schema = Schema::new();
    for attribute in [
        Attribute::new(
            KEY,
            Keyword::new("account", "key"),
            ValueType::Long,
            Cardinality::One,
        )
        .unique(Unique::Identity),
        Attribute::new(
            BALANCE,
            Keyword::new("account", "balance"),
            ValueType::Long,
            Cardinality::One,
        ),
        Attribute::new(
            PAYLOAD,
            Keyword::new("account", "payload"),
            ValueType::String,
            Cardinality::One,
        ),
    ] {
        schema.install(attribute)?;
    }
    edb_core::storage::BlockDatabase::create(&scoped_config, "read-load", schema)?;
    let role_url = |role: &str| parameter(&parameter(&scoped, "user", role), "password", &password);
    emit(format!(
        "CONFIG records={records} payload_per_record={payload} scan_payload_lower_bound={} peer_cache_entries={CACHE_ENTRIES} peer_cache_bytes={cache_bytes} writer_cache_bytes={} duration_ms={millis} sample_cap={MAX_SAMPLES} release={} schema={unique} transport={} ssd=false postgres_os_cache=uncontrolled_warm",
        records * payload,
        4 * 1024 * 1024,
        !cfg!(debug_assertions),
        if config.tls_required() {
            "verified_tls"
        } else {
            "plaintext"
        }
    ));
    emit(format!(
        "HOST available_parallelism={} cpu_max={:?}",
        std::thread::available_parallelism()?.get(),
        std::fs::read_to_string("/sys/fs/cgroup/cpu.max")
            .unwrap_or_else(|_| "unavailable".into())
            .trim()
    ));
    let mut writer = Process::spawn(
        "writer",
        &role_url(&writer_role),
        "read-load",
        records,
        payload,
    )?;
    report("seed", &[writer.metric()?], lifetime.start.elapsed());
    if writer.line()? != "READY" {
        return Err("writer not ready".into());
    }
    let mut reader_lifetimes = Vec::new();
    for count in [1, 2, 4] {
        let mut readers = (0..count)
            .map(|_| {
                Process::spawn(
                    "reader",
                    &role_url(&peer_role),
                    "read-load",
                    records,
                    payload,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        for reader in &readers {
            if reader.line()? != "BOOT" {
                return Err("reader missing boot".into());
            }
        }
        let cold_start = Instant::now();
        for reader in &mut readers {
            reader.command("open")?;
        }
        let cold = readers
            .iter()
            .map(Process::metric)
            .collect::<Result<Vec<_>>>()?;
        report(&format!("cold_{count}"), &cold, cold_start.elapsed());
        for reader in &readers {
            if reader.line()? != "READY" {
                return Err("reader not ready".into());
            }
        }
        let warm_start = Instant::now();
        writer.command(&format!("idle {millis}"))?;
        for reader in &mut readers {
            reader.command(&format!("warm {millis}"))?;
        }
        let warm = readers
            .iter()
            .map(Process::metric)
            .collect::<Result<Vec<_>>>()?;
        let warm_writer = writer.metric()?;
        let elapsed = warm_start.elapsed();
        report(&format!("warm_{count}"), &warm, elapsed);
        let mut all = warm;
        all.push(warm_writer);
        report(&format!("warm_{count}_all"), &all, elapsed);

        let scan_open_start = Instant::now();
        let mut scanner = Process::spawn(
            "reader",
            &role_url(&peer_role),
            "read-load",
            records,
            payload,
        )?;
        if scanner.line()? != "BOOT" {
            return Err("scanner missing boot".into());
        }
        scanner.command("open")?;
        let scan_cold = scanner.metric()?;
        report(
            &format!("scan_open_{count}"),
            &[scan_cold],
            scan_open_start.elapsed(),
        );
        if scanner.line()? != "READY" {
            return Err("scanner not ready".into());
        }
        let mixed_start = Instant::now();
        writer.command(&format!("write {millis}"))?;
        scanner.command(&format!("scan {millis}"))?;
        for reader in &mut readers {
            reader.command(&format!("mixed {millis}"))?;
        }
        let mixed = readers
            .iter()
            .map(Process::metric)
            .collect::<Result<Vec<_>>>()?;
        let scan = scanner.metric()?;
        let writes = writer.metric()?;
        let elapsed = mixed_start.elapsed();
        report(&format!("mixed_{count}_ordinary"), &mixed, elapsed);
        report(
            &format!("mixed_{count}_analytics"),
            std::slice::from_ref(&scan),
            elapsed,
        );
        report(
            &format!("mixed_{count}_writer"),
            std::slice::from_ref(&writes),
            elapsed,
        );
        let mut all = mixed;
        all.extend([scan, writes]);
        report(&format!("mixed_{count}_all"), &all, elapsed);
        for reader in readers {
            reader_lifetimes.push(reader.stop()?);
        }
        reader_lifetimes.push(scanner.stop()?);
    }
    let writer_lifetime = writer.stop()?;
    reader_lifetimes.push(writer_lifetime);
    report(
        "all_process_lifetimes",
        &reader_lifetimes,
        lifetime.start.elapsed(),
    );
    fixture.cleanup()?;
    drop(fixture);
    lifetime.finish("controller_setup_cleanup", &[], &SqlIoStats::default(), "");
    emit("COMPLETE isolated_schema_and_roles_removed=true");
    Ok(())
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let [role, database, records, payload] = args.as_slice() {
        if role == "reader" {
            return reader(database, records.parse()?, payload.parse()?);
        }
        if role == "writer" {
            return writer(database, records.parse()?, payload.parse()?);
        }
    }
    let mut records = 2048;
    let mut millis = 1500;
    let mut payload = 512;
    for pair in args.chunks(2) {
        let [name, value] = pair else {
            return Err("expected --records N --duration-ms N --payload-bytes N".into());
        };
        match name.as_str() {
            "--records" => records = value.parse()?,
            "--duration-ms" => millis = value.parse()?,
            "--payload-bytes" => payload = value.parse()?,
            _ => return Err("unknown campaign option".into()),
        }
    }
    if !(4..=8192).contains(&records)
        || !(100..=10_000).contains(&millis)
        || !(64..=4096).contains(&payload)
    {
        return Err(
            "campaign bounds: records 4..8192, duration 100..10000ms, payload 64..4096 bytes"
                .into(),
        );
    }
    campaign(records, millis, payload)
}
