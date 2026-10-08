//! Independent writer + two submitting peers, bounded caches, and real SIGKILL recovery.
//! EDB_POSTGRES_URL='host=... user=... dbname=...' cargo run --release --example scale_workflow
//! EDB_SCALE_RECORDS=100000 EDB_SCALE_BATCH=100 selects the measured workload.
//! Small smoke runs are permitted but explicitly do not establish cache-exceeding scale.
//! EDB_SCALE_PROBE_TIMEOUT_MS=5 times out each peer's first batch response.
//! Retries retain the original request: server timeout 30s, reconciliation 15min,
//! EDB_SCALE_IMPORT_TIMEOUT_SECS (default 7200) bounds the whole import.
//! SQL data is retained; this example never restarts PostgreSQL or invokes global GC.
use edb_core::{
    Attribute, AttributeName, BackgroundIndexingConfig, CallableRef, CapacityLimits, Cardinality,
    Clause, CommittedTransaction, Connection, DataPattern, DatabaseValue, EntityRef, ErrorCategory,
    FindElement, FindSpec, IndexPrefix, Instruction, Keyword, LocalTransactionServer,
    LocalTransportConfig, Peer, PostgresConnectionConfig, PostgresOperator, Program, ProgramCall,
    ProgramKind, PullAttribute, PullPattern, Query, QueryControl, QueryResult, QueryValue, Schema,
    Term, TransactionRequest, TransactionService, TransactionServiceConfig, TxForm, TxOp, Unique,
    Value, ValueType, Variable,
};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const KEY: u32 = 1000;
const CATEGORY: u32 = 1001;
const BALANCE: u32 = 1002;
const PAYLOAD: u32 = 1003;
const PEER_ENTRIES: usize = 64;
const PEER_BYTES: usize = 1024 * 1024;
const WRITER_ENTRIES: usize = 256;
const WRITER_BYTES: usize = 4 * 1024 * 1024;
const RECENT_THRESHOLD: u64 = 2 * 1024 * 1024;
const RECENT_MAX: u64 = 8 * 1024 * 1024;
const WAIT: Duration = Duration::from_secs(180);
const START_WAIT: Duration = Duration::from_secs(45);
const FINAL_BALANCE: i64 = 777777;
const SERVER_TIMEOUT: Duration = Duration::from_secs(30);
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

fn import_timeout() -> Result<Duration> {
    let seconds = std::env::var("EDB_SCALE_IMPORT_TIMEOUT_SECS")
        .unwrap_or_else(|_| "7200".into())
        .parse::<u64>()?;
    if !(1..=86400).contains(&seconds) {
        return Err("import timeout must be 1..=86400 seconds".into());
    }
    Ok(Duration::from_secs(seconds))
}

#[derive(Default, Debug)]
struct SubmissionStats {
    attempts: u64,
    unknown_outcomes: u64,
    busy_admissions: u64,
    replayed_receipts: u64,
}

/// An elapsed response deadline says nothing about rollback. Preserve all
/// overrides and tempid spellings until the original request's outcome is known.
fn submit_confirmed(
    connection: &Connection,
    endpoint: &str,
    request: TransactionRequest,
    overall_deadline: Instant,
    first_timeout: Option<Duration>,
    expected_existing_receipt: bool,
    stats: &mut SubmissionStats,
) -> Result<CommittedTransaction> {
    let deadline = overall_deadline.min(Instant::now() + RECONCILE_TIMEOUT);
    let mut uncertain = false;
    let mut attempts = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("request remains unresolved at reconciliation deadline; preserve its identity before resuming".into());
        }
        let timeout = if attempts == 0 {
            first_timeout.unwrap_or(WAIT)
        } else {
            WAIT
        };
        attempts += 1;
        stats.attempts += 1;
        match connection.transact_socket(endpoint, request.clone(), timeout.min(remaining)) {
            Ok(committed) => {
                assert!(
                    !committed.replayed || uncertain || expected_existing_receipt,
                    "a never-uncertain fresh request unexpectedly replayed"
                );
                if committed.replayed {
                    stats.replayed_receipts += 1;
                }
                if let Ok(report) = &committed.report {
                    assert_eq!(report.basis_t, committed.basis_t);
                    assert_eq!(report.db_before.basis_t() + 1, report.basis_t);
                    assert_eq!(report.db_after.basis_t(), report.basis_t);
                }
                return Ok(committed);
            }
            Err(error) => {
                let code = error
                    .details
                    .get("remote_code")
                    .map_or(error.code, String::as_str);
                if error.category == ErrorCategory::UnknownOutcome {
                    uncertain = true;
                    stats.unknown_outcomes += 1;
                } else if error.category == ErrorCategory::Busy
                    && matches!(
                        code,
                        "service/queue-full" | "service/index-backpressure" | "transport/full"
                    )
                {
                    stats.busy_admissions += 1;
                } else {
                    return Err(error.into());
                }
                emit(format!(
                    "RETRY pid={} attempt={attempts} category={:?} code={code} remaining_ms={} stats={stats:?}",
                    std::process::id(),
                    error.category,
                    deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis()
                ))?;
                std::thread::sleep(
                    Duration::from_millis(250)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
    }
}

/// Own each child for its entire useful lifetime, including error unwinding.
struct Process {
    child: Child,
    lines: mpsc::Receiver<String>,
}
impl Process {
    fn spawn(args: &[&str]) -> Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("missing child stdout")?;
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self { child, lines })
    }

    fn command(&mut self, command: &str) -> Result<()> {
        writeln!(
            self.child.stdin.as_mut().ok_or("missing child stdin")?,
            "{command}"
        )?;
        Ok(())
    }

    fn line(&self, timeout: Duration) -> Result<String> {
        Ok(self.lines.recv_timeout(timeout)?)
    }

    fn stop(&mut self) -> Result<()> {
        self.command("stop")?;
        let deadline = Instant::now() + WAIT;
        loop {
            for line in self.lines.try_iter() {
                println!("{line}");
            }
            if let Some(status) = self.child.try_wait()? {
                assert!(status.success(), "child exited unsuccessfully: {status}");
                for line in self.lines.try_iter() {
                    println!("{line}");
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("child shutdown exceeded deadline".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn emit(line: impl std::fmt::Display) -> Result<()> {
    println!("{line}");
    std::io::stdout().flush()?;
    Ok(())
}

fn memory() -> Result<(u64, u64)> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let read = |label: &str| -> Result<u64> {
        let line = status
            .lines()
            .find(|line| line.starts_with(label))
            .ok_or("missing /proc memory field")?;
        Ok(line
            .split_whitespace()
            .nth(1)
            .ok_or("missing /proc memory value")?
            .parse::<u64>()?
            * 1024)
    };
    Ok((read("VmRSS:")?, read("VmHWM:")?))
}

fn connection(postgres: &str, database: &str) -> Result<Connection> {
    Ok(Connection::connect_with_cache_limits(
        postgres,
        database,
        PEER_ENTRIES,
        PEER_BYTES,
    )?)
}

fn check_peer(connection: &Connection) {
    let cache = connection.cache_stats();
    assert!(cache.peak_entries <= PEER_ENTRIES);
    assert!(cache.peak_bytes <= PEER_BYTES);
}

fn account(database: &DatabaseValue, key: i64, expected_balance: i64) -> Result<u64> {
    let entity = Variable::new("entity")?;
    let balance = Variable::new("balance")?;
    let query = Query::new(
        FindSpec::Tuple(vec![
            FindElement::Variable(entity.clone()),
            FindElement::Variable(balance.clone()),
        ]),
        vec![
            Clause::Pattern(Box::new(DataPattern::new(
                Term::Variable(entity.clone()),
                Term::Constant(Value::Long(i64::from(KEY))),
                Term::Constant(Value::Long(key)),
            ))),
            Clause::Pattern(Box::new(DataPattern::new(
                Term::Variable(entity),
                Term::Constant(Value::Long(i64::from(BALANCE))),
                Term::Variable(balance),
            ))),
        ],
    );
    let outcome = database.query(
        &query,
        &[],
        &QueryControl {
            timeout: Some(Duration::from_secs(10)),
            max_work: 1000,
            ..Default::default()
        },
    )?;
    assert!(
        outcome.stats.datoms_examined <= 8,
        "selective lookup became a broad scan: {:?}",
        outcome.stats
    );
    let QueryResult::Tuple(Some(values)) = outcome.result else {
        return Err("account query returned wrong shape".into());
    };
    let [
        QueryValue::Scalar(Value::Ref(entity)),
        QueryValue::Scalar(Value::Long(balance)),
    ] = values.as_slice()
    else {
        return Err("account query returned wrong types".into());
    };
    assert_eq!(*balance, expected_balance);
    let pulled = database.pull(
        &PullPattern::attributes(vec![
            PullAttribute::forward(AttributeName::Id(KEY)),
            PullAttribute::forward(AttributeName::Id(BALANCE)),
            PullAttribute::forward(AttributeName::Id(PAYLOAD)),
        ]),
        *entity,
    )?;
    let QueryValue::Map(ref fields) = pulled else {
        return Err("pull did not return a map".into());
    };
    assert!(fields.iter().any(|(name, value)| name
        == &QueryValue::Scalar(Value::Keyword(Keyword::new("account", "payload")))
        && matches!(value, QueryValue::Scalar(Value::String(text)) if text.len() == 256)));
    Ok(*entity)
}

fn final_request() -> TransactionRequest {
    TransactionRequest::from_forms(
        "scale-final-update",
        vec![set_balance(FINAL_BALANCE)],
        edb_core::EntityRef::Id(edb_core::DB_SOURCE_SYSTEM),
    )
}

fn set_balance(balance: i64) -> TxForm {
    TxForm::ProgramCall(ProgramCall {
        function: CallableRef::Database(EntityRef::Ident(Keyword::new("account", "set-balance"))),
        arguments: vec![
            EntityRef::Lookup {
                attribute: KEY,
                value: Value::Long(0),
            }
            .into(),
            Value::Long(balance).into(),
        ],
    })
}

#[derive(Default, Debug)]
struct Peaks {
    samples: u64,
    rss: u64,
    recent_bytes: u64,
    pressure_bytes: u64,
    cache_bytes: usize,
}

fn writer(postgres: &str, database: &str) -> Result<()> {
    let deadline = Instant::now() + START_WAIT;
    let service = loop {
        match TransactionService::start_with_indexing(
            TransactionServiceConfig {
                connection: edb_core::PostgresConnectionConfig::parse(postgres)?,
                database_id: database.to_owned(),
                holder_id: format!("scale-writer-{}", std::process::id()),
                lease_duration: Duration::from_secs(5),
                renew_interval: Duration::from_millis(100),
                queue_capacity: 16,
                capacity_limits: CapacityLimits {
                    writer_tree_cache_entries: WRITER_ENTRIES,
                    writer_tree_cache_bytes: WRITER_BYTES,
                    ..Default::default()
                },
            },
            BackgroundIndexingConfig {
                memory_index_threshold_bytes: RECENT_THRESHOLD,
                memory_index_max_bytes: RECENT_MAX,
            },
        ) {
            Ok(service) => break service,
            Err(error)
                if Instant::now() < deadline
                    && matches!(
                        error.category,
                        ErrorCategory::Busy | ErrorCategory::Unavailable
                    )
                    && error.code == "storage/writer-active" =>
            {
                std::thread::sleep(Duration::from_millis(100))
            }
            Err(error) => return Err(error.into()),
        }
    };
    let server = LocalTransactionServer::start(
        service.client(),
        LocalTransportConfig {
            request_timeout: SERVER_TIMEOUT,
            ..Default::default()
        },
    )?;
    emit(format!("ENDPOINT {}", server.endpoint().display()))?;
    let peaks = Arc::new(Mutex::new(Peaks::default()));
    let sampled = Arc::clone(&peaks);
    let client = service.client();
    let (stop, stopped) = mpsc::channel();
    let sampler = std::thread::spawn(move || {
        loop {
            let residence = client.writer_residency_stats();
            let background = client.background_indexing_stats();
            assert_eq!(residence.eager_database_values, 0);
            assert_eq!(residence.eager_current_facts, 0);
            assert_eq!(residence.eager_history_datoms, 0);
            assert!(residence.tree_cache_entries <= WRITER_ENTRIES);
            assert!(residence.tree_cache_bytes <= WRITER_BYTES);
            assert!(
                background.last_failure.is_none(),
                "background failure: {:?}",
                background.last_failure
            );
            let mut peak = sampled.lock().expect("sampler mutex");
            peak.samples += 1;
            peak.rss = peak.rss.max(memory().expect("Linux RSS").0);
            peak.recent_bytes = peak.recent_bytes.max(residence.recent_accounted_bytes);
            peak.pressure_bytes = peak.pressure_bytes.max(background.total_bytes);
            peak.cache_bytes = peak.cache_bytes.max(residence.tree_cache_bytes);
            drop(peak);
            if stopped.recv_timeout(Duration::from_millis(100)).is_ok() {
                break;
            }
        }
    });
    for command in std::io::stdin().lock().lines() {
        if sampler.is_finished() {
            sampler
                .join()
                .map_err(|_| "writer measurement assertion failed")?;
            return Err("writer measurement sampler stopped unexpectedly".into());
        }
        match command?.as_str() {
            "metrics" => emit(format!(
                "WRITER pid={} memory={:?} peaks={:?} residency={:?} background={:?} recovery={:?} service={:?}",
                std::process::id(),
                memory()?,
                peaks.lock().expect("peaks"),
                service.writer_residency_stats(),
                service.background_indexing_stats(),
                service.recovery_stats(),
                service.client().stats()
            ))?,
            "stop" => break,
            _ => return Err("unknown writer command".into()),
        }
    }
    stop.send(())?;
    sampler
        .join()
        .map_err(|_| "writer measurement sampler failed")?;
    emit(format!(
        "WRITER_FINAL pid={} memory={:?} peaks={:?} residency={:?} background={:?}",
        std::process::id(),
        memory()?,
        peaks.lock().expect("peaks"),
        service.writer_residency_stats(),
        service.background_indexing_stats()
    ))?;
    drop(server);
    service.shutdown();
    Ok(())
}

fn percentile(values: &mut [u128], percent: usize) -> u128 {
    values.sort_unstable();
    values[(values.len() * percent).div_ceil(100).saturating_sub(1)]
}

fn peer(
    postgres: &str,
    database: &str,
    endpoint: &str,
    lane: usize,
    records: usize,
    batch: usize,
) -> Result<()> {
    let peer = connection(postgres, database)?;
    let started = Instant::now();
    let overall_deadline = started + import_timeout()?;
    let probe_timeout = std::env::var("EDB_SCALE_PROBE_TIMEOUT_MS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .map(Duration::from_millis);
    if probe_timeout.is_some_and(|timeout| timeout.is_zero()) {
        return Err("probe timeout must be positive".into());
    }
    let mut submission_stats = SubmissionStats::default();
    let mut latencies = Vec::new();
    let mut payload_bytes = 0usize;
    let mut facts = 0usize;
    let mut old = None;
    let mut imported = 0usize;
    let owned = (records + 1 - lane) / 2;
    let batches = owned.div_ceil(batch);
    for batch_index in 0..batches {
        let mut operations = Vec::with_capacity(batch * 4);
        let first = batch_index * batch;
        let end = (first + batch).min(owned);
        for offset in first..end {
            let key = i64::try_from(offset * 2 + lane)?;
            let entity = EntityRef::Temp(format!("account-{key}"));
            let mut payload = format!("account-{key:020}-");
            payload.extend(std::iter::repeat_n('x', 256 - payload.len()));
            for (attribute, value) in [
                (KEY, Value::Long(key)),
                (CATEGORY, Value::Long(key % 16)),
                (BALANCE, Value::Long(key * 10)),
                (PAYLOAD, Value::String(payload)),
            ] {
                operations.push(TxOp::Add {
                    entity: entity.clone(),
                    attribute,
                    value: value.into(),
                });
            }
        }
        let began = Instant::now();
        let committed = submit_confirmed(
            &peer,
            endpoint,
            TransactionRequest::new(
                format!("scale-import-{lane}-{batch_index}"),
                operations,
                edb_core::EntityRef::Id(edb_core::DB_SOURCE_SYSTEM),
            ),
            overall_deadline,
            if batch_index == 0 {
                probe_timeout
            } else {
                None
            },
            false,
            &mut submission_stats,
        )?;
        let report = committed.report?;
        // Verify exact original entity names, basis, and complete submitted
        // business facts. Count each reconciled receipt once, never per attempt.
        assert_eq!(report.tempids.len(), end - first);
        let mut actual = std::collections::BTreeMap::new();
        for datom in &report.tx_data {
            if (KEY..=PAYLOAD).contains(&datom.attribute) {
                assert!(datom.added);
                assert_eq!(datom.tx, edb_core::t_to_tx(report.basis_t)?);
                assert!(
                    actual
                        .insert((datom.entity, datom.attribute), &datom.value)
                        .is_none()
                );
            }
        }
        assert_eq!(actual.len(), (end - first) * 4);
        for offset in first..end {
            let key = i64::try_from(offset * 2 + lane)?;
            let entity = report.tempids[&format!("account-{key}")];
            assert_eq!(actual[&(entity, KEY)], &Value::Long(key));
            assert_eq!(actual[&(entity, CATEGORY)], &Value::Long(key % 16));
            assert_eq!(actual[&(entity, BALANCE)], &Value::Long(key * 10));
            assert!(
                matches!(actual[&(entity, PAYLOAD)], Value::String(text) if text.len()==256 && text.starts_with(&format!("account-{key:020}-")))
            );
        }
        latencies.push(began.elapsed().as_micros());
        for datom in &report.tx_data {
            if (KEY..=PAYLOAD).contains(&datom.attribute) {
                facts += 1;
            }
            if datom.attribute == PAYLOAD {
                let Value::String(value) = &datom.value else {
                    return Err("payload type changed".into());
                };
                payload_bytes += value.len();
            }
        }
        if old.is_none() {
            old = Some(report.db_after.clone());
        }
        imported += end - first;
        if batch_index == 0
            || batch_index + 1 == batches
            || (batch_index + 1) % (batches / 4).max(1) == 0
        {
            let key = i64::try_from((end - 1) * 2 + lane)?;
            account(&report.db_after, key, key * 10)?;
            check_peer(&peer);
            emit(format!(
                "PEER_CHECKPOINT pid={} lane={lane} imported={imported} basis={} elapsed_ms={} memory={:?} cache={:?} recent={:?} loads={:?}",
                std::process::id(),
                report.basis_t,
                started.elapsed().as_millis(),
                memory()?,
                peer.cache_stats(),
                peer.recent_stats(),
                peer.load_stats()
            ))?;
        }
    }
    assert_eq!(imported, owned);
    assert_eq!(facts, owned * 4);
    assert_eq!(payload_bytes, owned * 256);
    if probe_timeout.is_some() {
        assert!(
            submission_stats.unknown_outcomes > 0,
            "response-timeout probe did not produce uncertainty"
        );
        assert!(
            submission_stats.replayed_receipts > 0,
            "response-timeout probe did not reconcile an already committed receipt"
        );
    }
    let old = old.ok_or("peer imported no records")?;
    let held = peer.sync()?;
    emit(format!(
        "PEER_DONE lane={lane} pid={} records={imported} transactions={batches} business_facts={facts} payload_utf8_bytes={payload_bytes} elapsed_ms={} records_per_sec={:.3} batch_us_p50={} batch_us_p95={} batch_us_p99={} submissions={submission_stats:?} memory={:?}",
        std::process::id(),
        started.elapsed().as_millis(),
        imported as f64 / started.elapsed().as_secs_f64(),
        percentile(&mut latencies, 50),
        percentile(&mut latencies, 95),
        percentile(&mut latencies, 99),
        memory()?
    ))?;
    for command in std::io::stdin().lock().lines() {
        match command?.as_str() {
            "check" => {
                account(&old, lane as i64, lane as i64 * 10)?;
                account(&held, lane as i64, lane as i64 * 10)?;
                check_peer(&peer);
                emit(format!(
                    "PEER_HELD_OK lane={lane} old_basis={} held_basis={} memory={:?}",
                    old.basis_t(),
                    held.basis_t(),
                    memory()?
                ))?;
            }
            "stop" => break,
            _ => return Err("unknown peer command".into()),
        }
    }
    emit(format!(
        "PEER_FINAL lane={lane} memory={:?} cache={:?} loads={:?}",
        memory()?,
        peer.cache_stats(),
        peer.load_stats()
    ))?;
    Ok(())
}

fn start_writer(database: &str) -> Result<(Process, String)> {
    let writer = Process::spawn(&["writer", database])?;
    let ready = writer.line(START_WAIT + Duration::from_secs(5))?;
    let endpoint = ready
        .strip_prefix("ENDPOINT ")
        .ok_or("writer did not publish endpoint")?
        .to_owned();
    Ok((writer, endpoint))
}

fn cold_warm(postgres: &str, database: &str, expected: i64, scale: bool) -> Result<()> {
    // Peer (without Connection's live observer) isolates these exact cursor I/O deltas.
    let peer = Peer::connect_with_cache_limits(postgres, database, PEER_ENTRIES, PEER_BYTES)?;
    let value = peer.db();
    let before = peer.load_stats();
    let began = Instant::now();
    account(&value, 0, expected)?;
    let cold_us = began.elapsed().as_micros();
    let cold = peer.load_stats();
    let began = Instant::now();
    account(&value, 0, expected)?;
    let warm_us = began.elapsed().as_micros();
    let warm = peer.load_stats();
    let cold_reads = cold.cursor_sql_reads - before.cursor_sql_reads;
    let warm_reads = warm.cursor_sql_reads - cold.cursor_sql_reads;
    assert!(warm_reads <= cold_reads);
    if scale {
        assert!(
            cold_reads > 0,
            "scaled cold access must exercise durable tree reads"
        );
    }
    assert!(peer.cache_stats().peak_bytes <= PEER_BYTES);
    println!(
        "COLD_WARM basis={} cold_us={cold_us} warm_us={warm_us} cold_sql_reads={cold_reads} warm_sql_reads={warm_reads} cold_sql_payload_bytes={} warm_sql_payload_bytes={} cache={:?} memory={:?}",
        value.basis_t(),
        cold.cursor_sql_read_bytes - before.cursor_sql_read_bytes,
        warm.cursor_sql_read_bytes - cold.cursor_sql_read_bytes,
        peer.cache_stats(),
        memory()?
    );
    Ok(())
}

fn main() -> Result<()> {
    let postgres = std::env::var("EDB_POSTGRES_URL")?;
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("writer") => return writer(&postgres, &arguments[1]),
        Some("peer") => {
            return peer(
                &postgres,
                &arguments[1],
                &arguments[2],
                arguments[3].parse()?,
                arguments[4].parse()?,
                arguments[5].parse()?,
            );
        }
        None => {}
        _ => return Err("usage: scale_workflow (children are launched automatically)".into()),
    }
    let records = std::env::var("EDB_SCALE_RECORDS")
        .unwrap_or_else(|_| "10000".into())
        .parse::<usize>()?;
    let batch = std::env::var("EDB_SCALE_BATCH")
        .unwrap_or_else(|_| "100".into())
        .parse::<usize>()?;
    if !(2..=10_000_000).contains(&records) || !(1..=1000).contains(&batch) {
        return Err("records must be 2..=10000000; batch 1..=1000".into());
    }
    let require_scale = std::env::var("EDB_SCALE_REQUIRE_CACHE_EXCEEDED")
        .map_or(records >= 25000, |value| value == "1");
    let database = format!(
        "scale-workflow-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let import_timeout = import_timeout()?;
    println!(
        "CONFIG database={database} records={records} batch_records={batch} submitting_peers=2 writer_cache_entries={WRITER_ENTRIES} writer_cache_bytes={WRITER_BYTES} peer_cache_entries={PEER_ENTRIES} peer_cache_bytes={PEER_BYTES} recent_threshold_bytes={RECENT_THRESHOLD} recent_max_bytes={RECENT_MAX} require_cache_exceeded={require_scale} server_timeout_secs={} client_attempt_timeout_secs={} reconcile_timeout_secs={} import_timeout_secs={}",
        SERVER_TIMEOUT.as_secs(),
        WAIT.as_secs(),
        RECONCILE_TIMEOUT.as_secs(),
        import_timeout.as_secs()
    );
    let storage = PostgresConnectionConfig::plaintext(&postgres);
    edb_core::storage::PgBlockStore::install(&storage)?;
    edb_core::storage::BlockDatabase::create(&storage, &database, Schema::new())?;
    let route = edb_core::DatabaseCatalog::connect_configured(&storage)?
        .resolve(&database)?
        .database_id;
    let deployed = Program {
        kind: ProgramKind::Transaction,
        arity: 2,
        instructions: vec![
            Instruction::PushArgument(0),
            Instruction::PushArgument(1),
            Instruction::EmitAdd(BALANCE),
            Instruction::Return,
        ],
    };
    let deployment = PostgresOperator::connect_configured(&storage)?.deploy_program(&deployed)?;
    let program = deployment;
    let (mut writer, endpoint) = start_writer(&database)?;
    let observer = connection(&postgres, &database)?;
    let mut category = Attribute::new(
        CATEGORY,
        Keyword::new("account", "category"),
        ValueType::Long,
        Cardinality::One,
    );
    category.indexed = true;
    let mut parent_submissions = SubmissionStats::default();
    let schema = submit_confirmed(
        &observer,
        &endpoint,
        TransactionRequest::new(
            "scale-schema",
            vec![
                TxOp::InstallAttribute(
                    Attribute::new(
                        KEY,
                        Keyword::new("account", "key"),
                        ValueType::Long,
                        Cardinality::One,
                    )
                    .unique(Unique::Identity),
                ),
                TxOp::InstallAttribute(category),
                TxOp::InstallAttribute(Attribute::new(
                    BALANCE,
                    Keyword::new("account", "balance"),
                    ValueType::Long,
                    Cardinality::One,
                )),
                TxOp::InstallAttribute(Attribute::new(
                    PAYLOAD,
                    Keyword::new("account", "payload"),
                    ValueType::String,
                    Cardinality::One,
                )),
                TxOp::Add {
                    entity: EntityRef::Temp("set-balance".into()),
                    attribute: edb_core::DB_IDENT as u32,
                    value: Value::Keyword(Keyword::new("account", "set-balance")).into(),
                },
                TxOp::Add {
                    entity: EntityRef::Temp("set-balance".into()),
                    attribute: edb_core::DB_FN as u32,
                    value: Value::Function(program).into(),
                },
            ],
            edb_core::EntityRef::Id(edb_core::DB_SOURCE_SYSTEM),
        ),
        Instant::now() + RECONCILE_TIMEOUT,
        None,
        false,
        &mut parent_submissions,
    )?
    .report?;
    let old_empty = observer.sync_to(schema.basis_t, WAIT)?;
    let record_text = records.to_string();
    let batch_text = batch.to_string();
    let mut peers = [
        Process::spawn(&["peer", &database, &endpoint, "0", &record_text, &batch_text])?,
        Process::spawn(&["peer", &database, &endpoint, "1", &record_text, &batch_text])?,
    ];
    let started = Instant::now();
    let mut done = [false; 2];
    let mut last_progress = Instant::now();
    let mut next_metrics = Instant::now() + Duration::from_secs(10);
    while !done.iter().all(|done| *done) {
        for (index, peer) in peers.iter_mut().enumerate() {
            for line in peer.lines.try_iter() {
                if line.starts_with("PEER_DONE ") {
                    done[index] = true;
                }
                println!("{line}");
                last_progress = Instant::now();
            }
            if let Some(status) = peer.child.try_wait()? {
                return Err(format!("peer exited before shutdown: {status}").into());
            }
        }
        if Instant::now() >= next_metrics {
            writer.command("metrics")?;
            println!("{}", writer.line(Duration::from_secs(10))?);
            next_metrics = Instant::now() + Duration::from_secs(10);
        }
        if last_progress.elapsed() > Duration::from_secs(900) {
            return Err("no peer checkpoint within 15 minutes".into());
        }
        if started.elapsed() > import_timeout {
            return Err("overall import deadline elapsed; retained database and request keys identify pending outcomes".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let import_transactions = records.div_ceil(2).div_ceil(batch) + (records / 2).div_ceil(batch);
    let imported = observer.sync_to(schema.basis_t + import_transactions as u64, WAIT)?;
    assert_eq!(
        imported.basis_t(),
        schema.basis_t + import_transactions as u64
    );
    let entity = account(&imported, 0, 0)?;
    assert!(old_empty.values(entity, KEY)?.is_empty());
    account(&imported, records as i64 - 1, (records as i64 - 1) * 10)?;
    let report = PostgresOperator::connect_configured(&storage)?.inspect_database(&route, true)?;
    assert!(report.healthy(), "{:?}", report.problems);
    let log_rows = report.metrics.transactions;
    let canonical_bytes = report
        .metrics
        .transaction_bytes
        .ok_or("deep inspection omitted log bytes")?;
    assert_eq!(log_rows, imported.basis_t());
    let scale = canonical_bytes > RECENT_MAX
        && canonical_bytes > WRITER_BYTES as u64
        && canonical_bytes > PEER_BYTES as u64;
    if require_scale {
        assert!(
            scale,
            "measured canonical log bytes do not exceed all declared cache/recent budgets"
        );
    }
    println!(
        "IMPORT_COMPLETE database={database} basis={} records={records} business_facts={} payload_utf8_bytes={} canonical_log_rows={log_rows} canonical_log_payload_bytes={canonical_bytes} cache_exceeded={scale} elapsed_ms={} records_per_sec={:.3} parent_memory={:?}",
        imported.basis_t(),
        records * 4,
        records * 256,
        started.elapsed().as_millis(),
        records as f64 / started.elapsed().as_secs_f64(),
        memory()?
    );
    cold_warm(&postgres, &database, 0, scale)?;
    let hypothetical = imported.with_forms(
        &[set_balance(123456)],
        imported.last_tx_instant().ok_or("no instant")? + 1,
        edb_core::EntityRef::Id(edb_core::DB_SOURCE_SYSTEM),
    )?;
    account(&hypothetical.db_after, 0, 123456)?;
    account(&imported, 0, 0)?;
    assert_eq!(observer.db().basis_t(), imported.basis_t());
    let committed = submit_confirmed(
        &observer,
        &endpoint,
        final_request(),
        Instant::now() + RECONCILE_TIMEOUT,
        None,
        false,
        &mut parent_submissions,
    )?;
    let report = committed.report?;
    let current = observer.sync_to(report.basis_t, WAIT)?;
    account(&current, 0, FINAL_BALANCE)?;
    account(&current.clone().as_of(imported.basis_t()), 0, 0)?;
    let history = current.clone().history();
    let history_datoms = history
        .prefix_cursor(&IndexPrefix::Eavt {
            entity,
            attribute: Some(BALANCE),
            value: None,
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(history_datoms.len(), 3);
    assert_eq!(history_datoms.iter().filter(|datom| datom.added).count(), 2);
    writer.command("metrics")?;
    println!("{}", writer.line(Duration::from_secs(10))?);
    let killed_pid = writer.child.id();
    writer.child.kill()?;
    let killed_status = writer.child.wait()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(killed_status.signal(), Some(9));
    }
    account(&imported, 0, 0)?;
    account(&current, 0, FINAL_BALANCE)?;
    for peer in &mut peers {
        peer.command("check")?;
        println!("{}", peer.line(Duration::from_secs(20))?);
    }
    println!("WRITER_SIGKILL pid={killed_pid} held_values_verified=true");
    let replacement_started = Instant::now();
    let (mut replacement, replacement_endpoint) = start_writer(&database)?;
    let retry = submit_confirmed(
        &observer,
        &replacement_endpoint,
        final_request(),
        Instant::now() + RECONCILE_TIMEOUT,
        None,
        true,
        &mut parent_submissions,
    )?;
    assert!(retry.replayed);
    assert_eq!(retry.basis_t, report.basis_t);
    let retried = retry.report?;
    assert_eq!(retried.tx_data, report.tx_data);
    account(&retried.db_before, 0, 0)?;
    account(&retried.db_after, 0, FINAL_BALANCE)?;
    replacement.command("metrics")?;
    println!("{}", replacement.line(Duration::from_secs(10))?);
    println!(
        "REPLACEMENT exact_retry=true elapsed_ms={} basis={}",
        replacement_started.elapsed().as_millis(),
        retried.basis_t
    );
    replacement.stop()?;
    for peer in &mut peers {
        peer.stop()?;
    }
    let reopened = connection(&postgres, &database)?;
    assert_eq!(reopened.identity(), observer.identity());
    assert_eq!(reopened.db().basis_t(), report.basis_t);
    account(&reopened.db(), 0, FINAL_BALANCE)?;
    account(&reopened.db().as_of(imported.basis_t()), 0, 0)?;
    check_peer(&reopened);
    check_peer(&observer);
    cold_warm(&postgres, &database, FINAL_BALANCE, scale)?;
    println!(
        "PASS database={database} basis={} records={records} import_transactions={import_transactions} cache_exceeded={scale} writer_offline=true exact_retry=true retained_history=true parent_submissions={parent_submissions:?} parent_memory={:?}",
        report.basis_t,
        memory()?
    );
    Ok(())
}
