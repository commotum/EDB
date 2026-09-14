//! Measured portable operations for an existing, stopped application database.
//! Required: ATOMIC_POSTGRES_URL, ATOMIC_DATABASE_ID, ATOMIC_RESTORE_POSTGRES_URL.
//! Optional: ATOMIC_BACKUP_DIRECTORY (otherwise a retained private temp dir).
//! ATOMIC_OPS_RESUME_MANIFEST resumes after a previously completed copy/repeat,
//! requiring its exact printed manifest hash and explicit backup directory.
//! The restore catalog/schema must be independent. Each phase gets a fresh
//! process, so Linux VmHWM describes that operation, not an earlier allocator.
//! Backups are retained; this example never deletes an existing repository.
use atomic_core::{
    Attribute, CallableRef, CapacityLimits, Cardinality, Clause, DataPattern, DatabaseValue,
    EntityRef, FindElement, FindSpec, IndexOrder, Keyword, Peer, PortableBackup,
    PostgresConnectionConfig, PostgresOperator, ProgramCall, Query, QueryControl, QueryResult,
    QueryValue, RuntimeValue, Term, TransactionRequest, TransactionService,
    TransactionServiceConfig, TxForm, TxOp, Value, ValueType, Variable, canonical_datom_bytes,
};
use postgres::{Client, NoTls};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Fields = BTreeMap<String, String>;
const PREFIX: &str = "OP_RESULT ";

fn main() -> Result<()> {
    if let Ok(phase) = std::env::var("ATOMIC_OPS_PHASE") {
        let baseline = memory();
        let cpu_before = process_cpu_micros();
        let context =
            atomic_core::OperationContext::new(atomic_core::OperationKind::Administration);
        let _scope = context.enter();
        let start = Instant::now();
        let result = run_phase(&phase);
        let io = context.snapshot();
        println!(
            "operations_phase={phase} outcome={} elapsed_ms={} cpu_us={} sql_calls={} result_cell_bytes={} sql_elapsed_ns={} baseline={baseline} final={}",
            if result.is_ok() { "passed" } else { "failed" },
            start.elapsed().as_millis(),
            process_cpu_micros().saturating_sub(cpu_before),
            io.sql_calls,
            io.result_cell_bytes,
            io.elapsed_nanos,
            memory()
        );
        for (key, value) in result? {
            println!("{PREFIX}{key}={value}");
        }
        return Ok(());
    }
    let source = std::env::var("ATOMIC_POSTGRES_URL")?;
    let target = std::env::var("ATOMIC_RESTORE_POSTGRES_URL")?;
    let database = std::env::var("ATOMIC_DATABASE_ID")?;
    let resume_manifest = std::env::var("ATOMIC_OPS_RESUME_MANIFEST").ok();
    if resume_manifest.is_some() && std::env::var_os("ATOMIC_BACKUP_DIRECTORY").is_none() {
        return Err("resuming requires the retained ATOMIC_BACKUP_DIRECTORY".into());
    }
    if catalog_identity(&source)? == catalog_identity(&target)? {
        return Err("restore must use a separate PostgreSQL catalog or schema".into());
    }

    let directory = std::env::var_os("ATOMIC_BACKUP_DIRECTORY")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| {
            let directory = tempfile::Builder::new()
                .prefix("atomic-operations-")
                .tempdir()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
            }
            Ok::<_, std::io::Error>(directory.keep())
        })?;
    println!(
        "operations_database={database} backup_directory={}",
        directory.display()
    );
    let start = Instant::now();
    let captured = child("source-native", &directory, &Fields::new())?;
    if let Some(manifest) = &resume_manifest {
        check_resume_point(&source, &database, &directory, &captured, manifest)?;
        println!(
            "operations_resumed_manifest={manifest} copy_and_repeat=previous_run standalone_verify=not_rerun restore_deep_verify=required"
        );
    } else {
        let first = child("backup-first", &directory, &captured)?;
        let repeated = child("backup-repeat", &directory, &captured)?;
        for key in ["basis", "manifest", "lineage", "generation"] {
            if first.get(key) != repeated.get(key) {
                return Err(format!("backup endpoint changed between passes: {key}").into());
            }
        }
        if repeated.get("objects_written").map(String::as_str) != Some("0") {
            return Err("unchanged repeat backup unexpectedly wrote immutable objects".into());
        }
        child("deep-verify", &directory, &captured)?;
    }
    child("restore", &directory, &captured)?;
    child("target-native", &directory, &captured)?;
    if captured.get("scale_fixture").map(String::as_str) == Some("true") {
        child("retry", &directory, &captured)?;
    }
    child("deep-inspect", &directory, &captured)?;
    // Detect a source writer accidentally restarted while the operation ran.
    let after = child("source-native", &directory, &captured)?;
    for key in ["basis", "frontier", "current", "history", "query"] {
        if captured.get(key) != after.get(key) {
            return Err(format!("source changed during operational acceptance: {key}").into());
        }
    }
    let (files, bytes) = repository_size(&directory)?;
    println!(
        "operations_acceptance=passed resumed={} elapsed_ms={} repository_files={files} repository_bytes={bytes} backup_retained={}",
        resume_manifest.is_some(),
        start.elapsed().as_millis(),
        directory.display()
    );
    Ok(())
}

fn check_resume_point(
    source: &str,
    database: &str,
    directory: &Path,
    captured: &Fields,
    manifest_hash: &str,
) -> Result<()> {
    let basis: u64 = captured
        .get("basis")
        .ok_or("source basis missing")?
        .parse()?;
    let entry = atomic_core::DatabaseCatalog::connect(source)?.resolve(database)?;
    let report = PostgresOperator::connect(source)?.inspect_database(&entry.database_id, false)?;
    let lineage = entry.lineage_id;
    let generation = report.metrics.generation;
    if report.metrics.basis_t != basis {
        return Err("source advanced before resume-point validation".into());
    }
    // The basis-only restore API chooses the latest generation at that basis.
    // Bind that exact selection before provisioning or changing the target.
    let point = PortableBackup::list_backup_points(directory)?
        .into_iter()
        .filter(|point| point.basis_t == basis)
        .max_by_key(|point| point.log_generation)
        .ok_or("retained repository does not contain the captured source basis")?;
    if point.lineage_id != lineage
        || point.log_generation != generation
        || hex(&point.manifest_hash) != manifest_hash
    {
        return Err("resume manifest does not identify the exact current source point".into());
    }
    // Listing is only an identity guard. Restore still performs the complete
    // semantic proof; target fingerprints/retry/integrity and source-unchanged
    // checks below remain mandatory. Copy timings come from the earlier run.
    Ok(())
}

fn child(phase: &str, directory: &Path, expected: &Fields) -> Result<Fields> {
    println!("operations_phase_start={phase}");
    let mut command = Command::new(std::env::current_exe()?);
    command
        .env("ATOMIC_OPS_PHASE", phase)
        .env("ATOMIC_BACKUP_DIRECTORY", directory)
        .stderr(Stdio::inherit());
    for (key, value) in expected {
        command.env(format!("ATOMIC_OPS_EXPECT_{}", key.to_uppercase()), value);
    }
    let output = command.output()?;
    let output_text = String::from_utf8(output.stdout)?;
    print!("{output_text}");
    if !output.status.success() {
        return Err(format!("operations phase {phase} failed: {}", output.status).into());
    }
    Ok(output_text
        .lines()
        .filter_map(|line| line.strip_prefix(PREFIX)?.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect())
}

fn run_phase(phase: &str) -> Result<Fields> {
    let source = std::env::var("ATOMIC_POSTGRES_URL")?;
    let target = std::env::var("ATOMIC_RESTORE_POSTGRES_URL")?;
    let database = std::env::var("ATOMIC_DATABASE_ID")?;
    let directory = PathBuf::from(
        std::env::var_os("ATOMIC_BACKUP_DIRECTORY").ok_or("backup directory missing")?,
    );
    let mut fields = Fields::new();
    match phase {
        "fixture-seed" => {
            seed_maintenance_fixture(&source, &database)?;
        }
        "source-native" | "target-native" => {
            let connection = if phase == "source-native" {
                &source
            } else {
                &target
            };
            fields = capture_native(connection, &database)?;
            if phase == "target-native" {
                for key in ["basis", "frontier", "current", "history", "query"] {
                    if fields.get(key) != Some(&expected(key)?) {
                        return Err(format!("restored native information differs: {key}").into());
                    }
                }
            }
        }
        "backup-first" | "backup-repeat" => {
            let point = PortableBackup::connect(&source)?.backup_database(&database, &directory)?;
            if point.basis_t.to_string() != expected("basis")? {
                return Err("backup basis differs from captured stopped source".into());
            }
            fields.insert("basis".into(), point.basis_t.to_string());
            fields.insert("manifest".into(), hex(&point.manifest_hash));
            fields.insert("lineage".into(), point.lineage_id);
            fields.insert("generation".into(), point.log_generation.to_string());
            fields.insert("objects_written".into(), point.objects_written.to_string());
            fields.insert("objects_reused".into(), point.objects_reused.to_string());
            let (files, bytes) = repository_size(&directory)?;
            fields.insert("repository_files".into(), files.to_string());
            fields.insert("repository_bytes".into(), bytes.to_string());
        }
        "deep-verify" => {
            let verified =
                PortableBackup::verify_backup(&directory, expected("basis")?.parse()?, true)?;
            fields.insert("objects_read".into(), verified.objects_read.to_string());
            fields.insert("basis".into(), verified.database.basis_t().to_string());
        }
        "restore" => {
            atomic_core::storage::PgBlockStore::install(&PostgresConnectionConfig::plaintext(
                &target,
            ))?;

            let restored = PortableBackup::connect(&target)?.restore_backup(
                &directory,
                expected("basis")?.parse()?,
                &database,
            )?;
            if restored.point.basis_t.to_string() != expected("basis")? {
                return Err("restore returned a different basis".into());
            }
            fields.insert("basis".into(), restored.point.basis_t.to_string());
        }
        "retry" => retry_scale_request(&target, &database, &mut fields)?,
        "deep-inspect" => {
            let entry = atomic_core::DatabaseCatalog::connect(&target)?.resolve(&database)?;
            let report =
                PostgresOperator::connect(&target)?.inspect_database(&entry.database_id, true)?;
            if !report.healthy() {
                return Err(format!("restored integrity failure: {:?}", report.problems).into());
            }
            println!("operations_integrity_metrics={:?}", report.metrics);
            fields.insert("integrity".into(), "healthy".into());
        }
        _ => return Err(format!("unknown operations phase: {phase}").into()),
    }
    Ok(fields)
}

/// Opt-in repeatable maintenance fixture. It creates only the explicitly named
/// logical database in an already selected source catalog; no GC or deletion.
/// Use an empty disposable schema. Rows carry two attributes, 256-byte strings,
/// and a retained publication after each eight 128-entity transactions.
fn seed_maintenance_fixture(connection: &str, database: &str) -> Result<()> {
    let records = std::env::var("ATOMIC_OPS_PROFILE_RECORDS")?.parse::<usize>()?;
    if records == 0 || records > 100_000 || records % 128 != 0 {
        return Err(
            "maintenance profile records must be a positive multiple of128, at most100000".into(),
        );
    }
    let config = PostgresConnectionConfig::plaintext(connection);
    atomic_core::storage::PgBlockStore::install(&config)?;
    let mut schema = atomic_core::Schema::new();
    let mut key = Attribute::new(
        1000,
        Keyword::new("maintenance", "key"),
        ValueType::Long,
        Cardinality::One,
    );
    key.indexed = true;
    schema.install(key)?;
    schema.install(Attribute::new(
        1001,
        Keyword::new("maintenance", "payload"),
        ValueType::String,
        Cardinality::One,
    ))?;
    atomic_core::storage::BlockDatabase::create(&config, database, schema)?;
    let mut basis = Peer::connect(connection, database, 8)?.basis_t();
    for publication in (0..records).step_by(1024) {
        let service = TransactionService::start(TransactionServiceConfig {
            connection: atomic_core::PostgresConnectionConfig::parse(connection)?,
            database_id: database.to_owned(),
            holder_id: format!("maintenance-profile-{}", std::process::id()),
            lease_duration: Duration::from_secs(5),
            renew_interval: Duration::from_millis(100),
            queue_capacity: 16,
            capacity_limits: CapacityLimits::default(),
        })?;
        for batch in (publication..records.min(publication + 1024)).step_by(128) {
            let mut operations = Vec::new();
            for n in batch..batch + 128 {
                let entity = EntityRef::Temp(format!("entity-{n}"));
                operations.push(TxOp::Add {
                    entity: entity.clone(),
                    attribute: 1000,
                    value: Value::Long(n as i64).into(),
                });
                operations.push(TxOp::Add {
                    entity,
                    attribute: 1001,
                    value: Value::String(format!("{n:016x}{}", "x".repeat(240))).into(),
                });
            }
            let request = TransactionRequest::new(format!("maintenance-{batch}"), operations)
                .comparing_basis(basis)
                .with_tx_instant((basis as i64 + 1) * 1000);
            let outcome = service
                .client()
                .transact(request, Duration::from_secs(120))?;
            basis += 1;
            if outcome.basis_t != basis {
                return Err("maintenance seed basis diverged".into());
            }
        }
        service.shutdown();
        let entry = atomic_core::DatabaseCatalog::connect(connection)?.resolve(database)?;
        PostgresOperator::connect(connection)?.consolidate_database(&entry.database_id)?;
        println!(
            "maintenance_seed records={} basis_t={basis}",
            records.min(publication + 1024)
        );
    }
    Ok(())
}

fn process_cpu_micros() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return 0;
    }
    let usage = unsafe { usage.assume_init() };
    [usage.ru_utime, usage.ru_stime]
        .iter()
        .map(|t| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64)
        .sum()
}

fn capture_native(connection: &str, database: &str) -> Result<Fields> {
    let open_start = Instant::now();
    let peer = Peer::connect_with_cache_limits(connection, database, 8, 256 * 1024)?;
    let open_ms = open_start.elapsed().as_millis();
    let value = peer.database_value();
    let scale = value
        .schema()
        .attribute(1_000)
        .is_ok_and(|attribute| attribute.ident == Keyword::new("account", "key"));
    let (entity, attribute) = if scale {
        (
            value
                .lookup(1_000, &Value::Long(0))?
                .ok_or("scale account 0 missing")?,
            1_002,
        )
    } else {
        let first = value
            .scan_cursor(IndexOrder::Eavt)?
            .next()
            .transpose()?
            .ok_or("database contains no facts")?;
        (first.entity, first.attribute)
    };
    let variable = Variable::new("value")?;
    let query = Query::new(
        FindSpec::Collection(FindElement::Variable(variable.clone())),
        vec![Clause::Pattern(Box::new(DataPattern::new(
            Term::Constant(Value::Ref(entity)),
            Term::Constant(Value::Keyword(
                value.schema().attribute(attribute)?.ident.clone(),
            )),
            Term::Variable(variable),
        )))],
    );
    let cold_start = Instant::now();
    let cold = value.query(&query, &[], &QueryControl::default())?;
    let cold_us = cold_start.elapsed().as_micros();
    let warm_start = Instant::now();
    let warm = value.query(&query, &[], &QueryControl::default())?;
    let warm_us = warm_start.elapsed().as_micros();
    assert_eq!(cold.result, warm.result);
    let expected_result = QueryResult::Collection(
        value
            .values(entity, attribute)?
            .into_iter()
            .map(QueryValue::Scalar)
            .collect(),
    );
    assert_eq!(cold.result, expected_result);
    if scale {
        assert_eq!(
            cold.result,
            QueryResult::Collection(vec![QueryValue::Scalar(Value::Long(777_777))])
        );
    }
    println!(
        "operations_native_open_ms={open_ms} cold_query_us={cold_us} warm_query_us={warm_us} cold_query_stats={:?} warm_query_stats={:?}",
        cold.stats, warm.stats
    );
    let query_hash = hex(&Sha256::digest(format!("{:?}", cold.result).as_bytes()));
    let current = fingerprint(&value)?;
    let history = fingerprint(&value.clone().history())?;
    let stats = peer.load_stats();
    println!(
        "operations_native_load={stats:?} cache={:?}",
        peer.cache_stats()
    );
    Ok(Fields::from([
        ("basis".into(), value.basis_t().to_string()),
        ("frontier".into(), value.eidx_frontier().to_string()),
        ("current".into(), current),
        ("history".into(), history),
        ("query".into(), query_hash),
        ("scale_fixture".into(), scale.to_string()),
    ]))
}

fn fingerprint(database: &DatabaseValue) -> Result<String> {
    let mut hash = Sha256::new();
    let mut count = 0_u64;
    for datom in database.scan_cursor(IndexOrder::Eavt)? {
        // Canonical codec rather than tree packing or Debug formatting. The
        // one-datom envelope is length-delimited before entering the stream.
        let encoded = canonical_datom_bytes(&datom?)?;
        hash.update((encoded.len() as u64).to_be_bytes());
        hash.update(encoded);
        count += 1;
    }
    Ok(format!("{count}:{}", hex(&hash.finalize())))
}

fn retry_scale_request(connection: &str, database: &str, fields: &mut Fields) -> Result<()> {
    let writer = TransactionService::start(TransactionServiceConfig {
        connection: atomic_core::PostgresConnectionConfig::parse(connection)?,
        database_id: database.into(),
        holder_id: format!("operations-retry-{}", std::process::id()),
        lease_duration: Duration::from_secs(10),
        renew_interval: Duration::from_millis(100),
        queue_capacity: 4,
        capacity_limits: CapacityLimits::default(),
    })?;
    let request = TransactionRequest::from_forms(
        "scale-final-update",
        vec![TxForm::ProgramCall(ProgramCall {
            function: CallableRef::Database(EntityRef::Ident(Keyword::new(
                "account",
                "set-balance",
            ))),
            arguments: vec![
                RuntimeValue::Entity(EntityRef::Lookup {
                    attribute: 1_000,
                    value: Value::Long(0),
                }),
                Value::Long(777_777).into(),
            ],
        })],
    );
    let report = writer.client().transact(request, Duration::from_secs(60))?;
    assert_eq!(report.basis_t.to_string(), expected("basis")?);
    let entity = report
        .db_after
        .lookup(1_000, &Value::Long(0))?
        .ok_or("retry account missing")?;
    assert_eq!(
        report.db_before.values(entity, 1_002)?,
        vec![Value::Long(0)]
    );
    assert_eq!(
        report.db_after.values(entity, 1_002)?,
        vec![Value::Long(777_777)]
    );
    fields.insert("retry_original_basis".into(), report.basis_t.to_string());
    drop(report);
    writer.shutdown();
    assert_eq!(
        Peer::connect(connection, database, 8)?
            .basis_t()
            .to_string(),
        expected("basis")?
    );
    Ok(())
}

fn expected(key: &str) -> Result<String> {
    Ok(std::env::var(format!(
        "ATOMIC_OPS_EXPECT_{}",
        key.to_uppercase()
    ))?)
}

fn catalog_identity(connection: &str) -> Result<(String, String, String)> {
    let row = Client::connect(connection, NoTls)?.query_one(
        "SELECT system_identifier::text, current_database(), current_schema() FROM pg_control_system()", &[],
    )?;
    Ok((row.get(0), row.get(1), row.get(2)))
}

fn repository_size(directory: &Path) -> Result<(u64, u64)> {
    let mut pending = vec![directory.to_owned()];
    let (mut files, mut bytes) = (0, 0);
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let metadata = entry.path().symlink_metadata()?;
            if metadata.file_type().is_symlink() {
                return Err("backup repository contains a symlink".into());
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                files += 1;
                bytes += metadata.len();
            }
        }
    }
    Ok((files, bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn memory() -> String {
    match std::fs::read_to_string("/proc/self/status") {
        Ok(status) => status
            .lines()
            .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(","),
        Err(_) => "RSS/HWM unavailable on this platform".into(),
    }
}
