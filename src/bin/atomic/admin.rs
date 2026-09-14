//! Explicit operator commands. Runtime startup never routes through this module.
use atomic_core::sql_io::SqlClient;
use atomic_core::storage::PgBlockStore;
use atomic_core::{
    BackupPoint, DatabaseCatalog, ErrorCategory, GarbageInventory, PortableBackup,
    PostgresConnectionConfig, PostgresOperator, Schema, SemanticError, postgres_config_from_env,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

pub const HELP: &str = "
Administrative commands (explicit credentials/targets; no automatic provisioning):
  atomic install [--writer-role ROLE --peer-role ROLE]
  atomic create --database NAME
  atomic list-databases [--retired] [--after NAME-OR-STORAGE-ID] [--limit N]
  atomic rename --database NAME --new-name NAME [--lineage UUID --apply]
  atomic delete --database NAME [--lineage UUID --apply]
  atomic gc-deleted --storage-id ID --lineage UUID --postgres-database NAME
    --catalog-schema NAME --older-than-seconds N [--apply --batches N]
  atomic backup --database ID --repository PATH [--maintenance-pause-ms N]
  atomic list-backups --repository PATH
  atomic verify-backup --repository PATH --basis N --generation N [--presence-only]
  atomic restore --repository PATH --basis N --generation N --target-database ID
    [--maintenance-pause-ms N]
    --postgres-database NAME --catalog-schema NAME [--apply]
  atomic inspect --database NAME [--shallow]
  atomic gc --postgres-database NAME --catalog-schema NAME --older-than-seconds N
    [--apply] [--batches N]
  atomic fulltext-rebuild --database ID
    [--discard-manifest SHA256 --apply --batches N]

Restore and GC preview by default; --apply authorizes their writes. Restore's
ATOMIC_POSTGRES_URL is the DESTINATION, not the backup's source. All targets must
already have the current storage installed. Exact basis/generation are required;
zero is valid. GC is
catalog-wide, not scoped to one logical database; explicit retention is required.
Each GC batch bounds ownership and sweep steps, not elapsed time. Default
batches=1; incomplete output reports resumable work. The normal storage retention
grace is 30 days. Shorter explicit retention can invalidate held readers.
Offline list/verify need no PostgreSQL environment. Verification is deep unless
--presence-only; backup success alone is not deep semantic verification.
SIGINT/SIGTERM may leave committed resumable work: retry the same exact target
and point. Rebuild replaces only the derived search attachment. The explicit
discard digest guards the current index descriptor; old search remains readable
within the storage retention grace and is reclaimed by ordinary GC. There is no physical discard
phase (the accepted --batches limit needs just one rebuild).
See docs/08_operations/01_administration.md and docs/08_operations/00_deployment.md.

Create is idempotent; list is paginated (default 1000, maximum 4096). Rename/delete
preview by default. Apply requires the lineage printed by preview to guard name
reuse. Delete retires and fences a database; it does not reclaim storage. Already
captured values remain readable within storage retention. gc-deleted targets one retired storage
ID/lineage, admits a shared collection cycle and preserves still-owned shared content.
Cycle completion does not promise deletion of still-owned objects. See
docs/08_operations/02_database_lifecycle.md. These mutations require catalog owner authority.

Install creates two opaque tables in the selected empty schema. It does not
create the PostgreSQL database or schema. Grants add generic storage permissions to existing dedicated roles;
they do not revoke existing grants or audit inherited access. Peers can read
objects and references. Writers can also insert/protect objects and maintain references;
object deletion remains operator-only. References are trusted storage access,
not per-database SQL authorization.
";

pub fn dispatch(arguments: &[String]) -> Option<Result<(), SemanticError>> {
    let command = arguments.first()?.as_str();
    let (values, switches): (&[&str], &[&str]) = match command {
        "install" => (&["--writer-role", "--peer-role"], &[]),
        "create" | "status" | "consolidate" => (&["--database"], &[]),
        "list-databases" => (&["--after", "--limit"], &["--retired"]),
        "rename" => (&["--database", "--new-name", "--lineage"], &["--apply"]),
        "delete" => (&["--database", "--lineage"], &["--apply"]),
        "gc-deleted" => (
            &[
                "--storage-id",
                "--lineage",
                "--postgres-database",
                "--catalog-schema",
                "--older-than-seconds",
                "--batches",
            ],
            &["--apply"],
        ),
        "backup" => (
            &["--database", "--repository", "--maintenance-pause-ms"],
            &[],
        ),
        "list-backups" => (&["--repository"], &[]),
        "verify-backup" => (
            &["--repository", "--basis", "--generation"],
            &["--presence-only"],
        ),
        "restore" => (
            &[
                "--maintenance-pause-ms",
                "--repository",
                "--basis",
                "--generation",
                "--target-database",
                "--postgres-database",
                "--catalog-schema",
            ],
            &["--apply"],
        ),
        "inspect" => (&["--database"], &["--shallow"]),
        "gc" => (
            &[
                "--maintenance-pause-ms",
                "--postgres-database",
                "--catalog-schema",
                "--older-than-seconds",
                "--batches",
            ],
            &["--apply"],
        ),
        "fulltext-rebuild" => (
            &["--database", "--discard-manifest", "--batches"],
            &["--apply"],
        ),
        _ => return None,
    };
    Some(Arguments::parse(command, &arguments[1..], values, switches).and_then(run))
}

struct Arguments {
    command: String,
    values: BTreeMap<String, String>,
    switches: BTreeSet<String>,
}
impl Arguments {
    fn parse(
        command: &str,
        raw: &[String],
        values: &[&str],
        switches: &[&str],
    ) -> Result<Self, SemanticError> {
        let mut parsed = Self {
            command: command.into(),
            values: BTreeMap::new(),
            switches: BTreeSet::new(),
        };
        let mut iter = raw.iter();
        while let Some(flag) = iter.next() {
            if switches.contains(&flag.as_str()) {
                if !parsed.switches.insert(flag.clone()) {
                    return Err(usage("duplicate option"));
                }
            } else if values.contains(&flag.as_str()) {
                let value = iter
                    .next()
                    .filter(|value| !value.is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| usage("option requires a nonempty value"))?;
                if parsed.values.insert(flag.clone(), value.clone()).is_some() {
                    return Err(usage("duplicate option"));
                }
            } else {
                return Err(usage("unknown or misplaced option; run atomic --help"));
            }
        }
        let required: &[&str] = match command {
            "install" => &[],
            "list-databases" => &[],
            "rename" => &["--database", "--new-name"],
            "delete" => &["--database"],
            "gc-deleted" => &[
                "--storage-id",
                "--lineage",
                "--postgres-database",
                "--catalog-schema",
                "--older-than-seconds",
            ],
            "create" | "status" | "consolidate" | "inspect" | "fulltext-rebuild" => &["--database"],
            "backup" => &["--database", "--repository"],
            "list-backups" => &["--repository"],
            "verify-backup" => &["--repository", "--basis", "--generation"],
            "restore" => &[
                "--repository",
                "--basis",
                "--generation",
                "--target-database",
                "--postgres-database",
                "--catalog-schema",
            ],
            "gc" => &[
                "--postgres-database",
                "--catalog-schema",
                "--older-than-seconds",
            ],
            _ => unreachable!(),
        };
        for flag in required {
            parsed.required(flag)?;
        }
        for flag in [
            "--basis",
            "--generation",
            "--older-than-seconds",
            "--limit",
            "--maintenance-pause-ms",
        ] {
            if parsed.values.contains_key(flag) {
                parsed.number(flag)?;
            }
        }
        if parsed.values.contains_key("--batches") && parsed.number("--batches")? == 0 {
            return Err(usage("--batches requires a positive integer"));
        }
        if matches!(command, "install") {
            let writer = parsed.values.get("--writer-role");
            let peer = parsed.values.get("--peer-role");
            if writer.is_some() != peer.is_some() {
                return Err(usage(
                    "--writer-role and --peer-role must be supplied together",
                ));
            }
            if writer.is_some() && writer == peer {
                return Err(usage("writer and peer roles must be distinct"));
            }
        }
        if matches!(command, "rename" | "delete") && parsed.has("--apply") {
            parsed.required("--lineage")?;
        }
        if command == "list-databases"
            && parsed.values.contains_key("--limit")
            && !(1..=4096).contains(&parsed.number("--limit")?)
        {
            return Err(usage("--limit must be between 1 and 4096"));
        }
        if matches!(command, "gc" | "gc-deleted") {
            if parsed.number("--older-than-seconds")? > (i64::MAX as u64) / 1000 {
                return Err(usage(
                    "retention interval exceeds PostgreSQL millisecond range",
                ));
            }
            if !parsed.has("--apply") && parsed.values.contains_key("--batches") {
                return Err(usage(
                    "--batches requires --apply; preview is one read-only inventory",
                ));
            }
        }
        if command == "fulltext-rebuild" {
            let discard = parsed.values.contains_key("--discard-manifest");
            if discard {
                parse_digest(parsed.required("--discard-manifest")?)?;
            }
            if discard != parsed.has("--apply") {
                return Err(usage(
                    "derived discard requires both --discard-manifest and --apply",
                ));
            }
            if !discard && parsed.values.contains_key("--batches") {
                return Err(usage("--batches applies only to explicit derived discard"));
            }
        }
        Ok(parsed)
    }
    fn required(&self, flag: &str) -> Result<&str, SemanticError> {
        self.values
            .get(flag)
            .map(String::as_str)
            .ok_or_else(|| usage(format!("required option {flag} is missing")))
    }
    fn has(&self, flag: &str) -> bool {
        self.switches.contains(flag)
    }
    fn number(&self, flag: &str) -> Result<u64, SemanticError> {
        let value = self.required(flag)?;
        if !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(usage(format!("{flag} requires a nonnegative integer")));
        }
        value
            .parse()
            .map_err(|_| usage(format!("{flag} exceeds its supported integer range")))
    }
    fn batches(&self) -> Result<u64, SemanticError> {
        if self.values.contains_key("--batches") {
            self.number("--batches")
        } else {
            Ok(1)
        }
    }
}

fn usage(message: impl Into<String>) -> SemanticError {
    SemanticError::incorrect("cli/usage", message)
}

fn catalog_entry(label: &str, entry: &atomic_core::DatabaseCatalogEntry) {
    println!(
        "{label} name={:?} storage_id={:?} lineage={} retired={}",
        entry.name, entry.database_id, entry.lineage_id, entry.retired
    );
}
fn process_io() -> SemanticError {
    SemanticError::new(ErrorCategory::Unavailable, "cli/io", "process I/O failed")
}
fn progress(command: &str, phase: &str) -> Result<(), SemanticError> {
    println!("PROGRESS command={command} phase={phase}");
    std::io::stdout().flush().map_err(|_| process_io())
}
fn hex(hash: &[u8]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn parse_digest(text: &str) -> Result<[u8; 32], SemanticError> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(usage(
            "--discard-manifest requires exactly 64 hexadecimal characters",
        ));
    }
    let mut result = [0; 32];
    for (i, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
            .map_err(|_| usage("invalid manifest digest"))?;
    }
    Ok(result)
}
fn point(label: &str, point: &BackupPoint) {
    println!(
        "{label} lineage={} basis_t={} generation={} manifest={} objects_written={} objects_reused={}",
        point.lineage_id,
        point.basis_t,
        point.log_generation,
        hex(&point.manifest_hash),
        point.objects_written,
        point.objects_reused
    );
}
fn sql_client(config: &PostgresConnectionConfig) -> Result<SqlClient, SemanticError> {
    SqlClient::connect_with(|| config.connect())
}
fn query_error(_: postgres::Error) -> SemanticError {
    SemanticError::new(
        ErrorCategory::Unavailable,
        "cli/catalog-observation",
        "catalog observation failed",
    )
}
fn verify_target(config: &PostgresConnectionConfig, args: &Arguments) -> Result<(), SemanticError> {
    // The provider pins and fully qualifies the selected schema, and checks its
    // format there. Never approve a later relation found through search_path.
    let store = PgBlockStore::connect(config)?;
    let database: String = sql_client(config)?
        .query_one("SELECT current_database()", &[])
        .map_err(query_error)?
        .get(0);
    let schema = store.namespace();
    if database != args.required("--postgres-database")?
        || schema != args.required("--catalog-schema")?
    {
        return Err(SemanticError::new(
            ErrorCategory::Conflict,
            "cli/target-mismatch",
            "configured PostgreSQL database/catalog do not match the explicit target",
        ));
    }
    println!("TARGET postgres_database={database:?} catalog_schema={schema:?}");
    Ok(())
}
fn selected_point(args: &Arguments) -> Result<BackupPoint, SemanticError> {
    let basis = args.number("--basis")?;
    let generation = args.number("--generation")?;
    PortableBackup::list_backup_points(Path::new(args.required("--repository")?))?
        .into_iter()
        .find(|p| p.basis_t == basis && p.log_generation == generation)
        .ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::NotFound,
                "cli/backup-point-not-found",
                "repository does not contain the exact selected basis/generation",
            )
        })
}

fn run(args: Arguments) -> Result<(), SemanticError> {
    let started = Instant::now();
    if args.command == "list-backups" {
        let points = PortableBackup::list_backup_points(Path::new(args.required("--repository")?))?;
        for p in &points {
            point("BACKUP_POINT", p);
        }
        println!("BACKUPS count={}", points.len());
        return Ok(());
    }
    if args.command == "verify-backup" {
        progress("verify-backup", "verify")?;
        let directory = Path::new(args.required("--repository")?);
        if args.has("--presence-only") {
            let p = PortableBackup::verify_backup_point_presence(
                directory,
                args.number("--basis")?,
                args.number("--generation")?,
            )?;
            point("VERIFIED_PRESENCE", &p);
            println!(
                "VERIFICATION semantic=false elapsed_ms={}",
                started.elapsed().as_millis()
            );
        } else {
            let verified = PortableBackup::verify_backup_point(
                directory,
                args.number("--basis")?,
                args.number("--generation")?,
                true,
            )?;
            point("VERIFIED_DEEP", &verified.point);
            println!(
                "VERIFICATION semantic=true objects_read={} elapsed_ms={}",
                verified.objects_read,
                started.elapsed().as_millis()
            );
        }
        return Ok(());
    }
    let config = postgres_config_from_env()?;
    let maintenance = atomic_core::MaintenanceControl::new(
        Duration::from_millis(if args.values.contains_key("--maintenance-pause-ms") {
            args.number("--maintenance-pause-ms")?
        } else {
            0
        }),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )?;
    match args.command.as_str() {
        "install" => {
            PgBlockStore::install(&config)?;
            println!("INSTALLED storage_format=atomic/opaque-storage/1 tables=2");
            std::io::stdout().flush().map_err(|_| process_io())?;
            if let Some(writer) = args.values.get("--writer-role") {
                PgBlockStore::connect(&config)?
                    .grant_runtime_privileges(writer, args.required("--peer-role")?)?;
                println!("GRANTED runtime_roles=true additive=true object_delete=operator_only");
            }
        }
        "create" => {
            let id = args.required("--database")?;
            let result = DatabaseCatalog::connect_configured(&config)?
                .create_if_absent(id, Schema::new())?;
            catalog_entry(
                if result.created { "CREATED" } else { "EXISTS" },
                &result.database,
            );
        }
        "list-databases" => {
            let mut catalog = DatabaseCatalog::connect_configured(&config)?;
            let after = args.values.get("--after").map(String::as_str);
            let limit = if args.values.contains_key("--limit") {
                args.number("--limit")? as usize
            } else {
                1000
            };
            let entries = if args.has("--retired") {
                catalog.list_retired(after, limit)?
            } else {
                catalog.list(after, limit)?
            };
            for entry in &entries {
                catalog_entry("DATABASE", entry);
            }
            println!(
                "DATABASE_PAGE entries={} limit={limit} retired={} full_page={}",
                entries.len(),
                args.has("--retired"),
                entries.len() == limit
            );
        }
        "rename" | "delete" => {
            let mut catalog = DatabaseCatalog::connect_configured(&config)?;
            let name = args.required("--database")?;
            if !args.has("--apply") {
                let entry = catalog.resolve(name)?;
                catalog_entry("LIFECYCLE_PREVIEW", &entry);
                println!(
                    "LIFECYCLE action={} new_name={:?} applied=false",
                    args.command,
                    args.values.get("--new-name")
                );
            } else if args.command == "rename" {
                let entry = catalog.rename_checked(
                    name,
                    args.required("--new-name")?,
                    args.required("--lineage")?,
                )?;
                catalog_entry("RENAMED", &entry);
            } else if let Some(entry) = catalog.retire_checked(name, args.required("--lineage")?)? {
                catalog_entry("RETIRED", &entry);
                println!("DELETED reclaimed=false");
            } else {
                println!("DELETE unchanged=true active_name_absent=true");
            }
        }
        "gc-deleted" => {
            verify_target(&config, &args)?;
            let mut operator = PostgresOperator::connect_configured(&config)?;
            let storage_id = args.required("--storage-id")?;
            let lineage = args.required("--lineage")?;
            let age = Duration::from_secs(args.number("--older-than-seconds")?);
            let batches = if args.has("--apply") {
                args.batches()?
            } else {
                1
            };
            for batch in 1..=batches {
                let report = if args.has("--apply") {
                    operator.reclaim_retired_database(storage_id, lineage, age)?
                } else {
                    operator.preview_retired_database_reclamation(storage_id, lineage, age)?
                };
                println!(
                    "RECLAMATION storage_id={:?} lineage={} batch={batch} phase={:?} ownership_steps={} objects_examined={} objects_removed={} cycle_complete={} applied={} retained_owners=protected",
                    report.storage_id,
                    report.lineage_id,
                    report.phase,
                    report.collection.ownership_steps,
                    report.collection.objects_examined,
                    report.collection.objects_removed,
                    report.cycle_complete,
                    report.applied
                );
                std::io::stdout().flush().map_err(|_| process_io())?;
                if report.cycle_complete {
                    break;
                }
            }
        }
        "status" => {
            let entry = DatabaseCatalog::connect_configured(&config)?
                .resolve(args.required("--database")?)?;
            let status = PostgresOperator::connect_configured(&config)?
                .inspect_database(&entry.database_id, false)?;
            println!(
                "STATUS database={:?} lineage={} basis_t={} generation={}",
                args.required("--database")?,
                entry.lineage_id,
                status.metrics.basis_t,
                status.metrics.generation
            );
        }
        "consolidate" => {
            consolidate(&config, args.required("--database")?, &maintenance)?;
        }
        "backup" => {
            progress("backup", "capture")?;
            let p = PortableBackup::connect_configured(&config)?
                .with_maintenance_control(maintenance.clone())
                .backup_database(
                    args.required("--database")?,
                    Path::new(args.required("--repository")?),
                )?;
            point("BACKED_UP", &p);
            println!(
                "BACKUP semantic_verification=false elapsed_ms={}",
                started.elapsed().as_millis()
            );
        }
        "restore" => {
            let p = selected_point(&args)?;
            drop(PgBlockStore::connect(&config)?);
            verify_target(&config, &args)?;
            let directory = Path::new(args.required("--repository")?);
            if !args.has("--apply") {
                progress("restore", "preview-structure")?;
                PortableBackup::verify_backup_point_presence(
                    directory,
                    p.basis_t,
                    p.log_generation,
                )?;
                point("RESTORE_POINT", &p);
                println!(
                    "RESTORE_PREVIEW target_database={:?} applied=false target_compatibility=checked-on-apply elapsed_ms={}",
                    args.required("--target-database")?,
                    started.elapsed().as_millis()
                );
            } else {
                progress("restore", "copy-activate")?;
                let db = PortableBackup::connect_configured(&config)?
                    .with_maintenance_control(maintenance.clone())
                    .restore_backup_point(
                        directory,
                        p.basis_t,
                        p.log_generation,
                        args.required("--target-database")?,
                    )?;
                println!(
                    "RESTORED target_database={:?} lineage={} basis_t={} selected_generation={} elapsed_ms={}",
                    args.required("--target-database")?,
                    p.lineage_id,
                    db.point.basis_t,
                    p.log_generation,
                    started.elapsed().as_millis()
                );
            }
        }
        "inspect" => {
            progress("inspect", "consistent-snapshot")?;
            let deep = !args.has("--shallow");
            let entry = DatabaseCatalog::connect_configured(&config)?
                .resolve(args.required("--database")?)?;
            let report = PostgresOperator::connect_configured(&config)?
                .inspect_database(&entry.database_id, deep)?;
            let m = &report.metrics;
            println!(
                "INSPECT healthy={} deep_derived={} basis_t={} index_basis_t={} index_lag={} problems={} pending_avet_projections={} generation={} publication_revision={} reachable_objects={:?} elapsed_ms={}",
                report.healthy(),
                deep,
                m.basis_t,
                m.index_basis_t,
                m.index_lag,
                report.problems.len(),
                m.pending_avet_projections,
                m.generation,
                m.publication_revision,
                m.reachable_objects,
                started.elapsed().as_millis()
            );
            for problem in &report.problems {
                // Codes are diagnostic labels; messages may contain subject data.
                let code = if problem.code.len() <= 160
                    && problem
                        .code
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
                {
                    problem.code.as_str()
                } else {
                    "redacted"
                };
                println!("INTEGRITY_PROBLEM code={code}");
            }
            if !report.healthy() {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "cli/integrity-problems",
                    "inspection found integrity problems",
                ));
            }
        }
        "gc" => {
            verify_target(&config, &args)?;
            let age = Duration::from_secs(args.number("--older-than-seconds")?);
            let mut operator = PostgresOperator::connect_configured(&config)?
                .with_maintenance_control(maintenance.clone());
            println!(
                "GC_POLICY older_than_seconds={} short_retention={} scope=entire-catalog",
                age.as_secs(),
                age < atomic_core::RECOMMENDED_GARBAGE_COLLECTION_AGE
            );
            if args.has("--apply") {
                let mut complete = false;
                let mut advanced = 0;
                for batch in 1..=args.batches()? {
                    progress("gc", "collect-bounded-batch")?;
                    let report = operator.collect_garbage(age)?;
                    complete = report.phase
                        == Some(atomic_core::storage::ownership::CollectionPhase::Complete);
                    advanced = batch;
                    inventory("GC_APPLIED", batch, &report);
                    std::io::stdout().flush().map_err(|_| process_io())?;
                    if complete {
                        break;
                    }
                }
                println!(
                    "GC_PROGRESS batches={} cycle_complete={} resumable={} global_quiescence=not-established elapsed_ms={}",
                    advanced,
                    complete,
                    !complete,
                    started.elapsed().as_millis()
                );
            } else {
                inventory("GC_PREVIEW", 1, &operator.garbage_inventory(age)?);
            }
        }
        "fulltext-rebuild" => rebuild(&config, &args, &maintenance)?,
        _ => unreachable!("validated administrative command"),
    }
    if args.values.contains_key("--maintenance-pause-ms") {
        let stats = maintenance.stats();
        println!(
            "MAINTENANCE batches={} pauses={} paused_ms={}",
            stats.completed_batches,
            stats.pauses,
            stats.paused_nanos / 1_000_000
        );
    }
    Ok(())
}

fn inventory(label: &str, batch: u64, inventory: &GarbageInventory) {
    println!(
        "{label} batch={batch} applied={} phase={:?} sealed_epoch={:?} stored_objects={:?} stored_bytes={:?} pending_events={:?} ownership_steps={} newly_owned={} objects_examined={} objects_removed={} bytes_removed={}",
        inventory.applied,
        inventory.phase,
        inventory.sealed_epoch,
        inventory.stored_objects,
        inventory.stored_bytes,
        inventory.pending_events,
        inventory.collection.ownership_steps,
        inventory.collection.newly_owned_objects,
        inventory.collection.objects_examined,
        inventory.collection.objects_removed,
        inventory.collection.stored_bytes_removed,
    );
    if let Some(handoffs) = &inventory.report_handoffs {
        println!(
            "GC_REPORT_HANDOFFS batch={batch} examined={} removed={} page_complete={} next_cycle_may_have_work=true",
            handoffs.examined, handoffs.removed, handoffs.complete,
        );
    }
    if let Some(authorization) = &inventory.authorization {
        println!(
            "GC_AUTHORIZATION batch={batch} examined={} removed={} unsettled={} page_complete={} next_cycle_may_have_work=true",
            authorization.examined,
            authorization.removed,
            authorization.unsettled,
            authorization.complete,
        );
    }
}
pub(super) fn consolidate(
    config: &PostgresConnectionConfig,
    database: &str,
    control: &atomic_core::MaintenanceControl,
) -> Result<(), SemanticError> {
    let entry = DatabaseCatalog::connect_configured(config)?.resolve(database)?;
    let receipt = PostgresOperator::connect_configured(config)?
        .with_maintenance_control(control.clone())
        .consolidate_database(&entry.database_id)?;
    println!(
        "INDEXED basis_t={} input_datoms={} descriptor={} steps={} recovered={} retained_history_verified=false",
        receipt.basis_t,
        receipt.input_datoms,
        hex(&receipt.descriptor_id),
        receipt.steps,
        receipt.recovered,
    );
    println!("SEARCH basis_t={} status=checked", receipt.basis_t);
    Ok(())
}
fn rebuild(
    config: &PostgresConnectionConfig,
    args: &Arguments,
    control: &atomic_core::MaintenanceControl,
) -> Result<(), SemanticError> {
    let database = args.required("--database")?;
    let entry = DatabaseCatalog::connect_configured(config)?.resolve(database)?;
    let expected = args
        .values
        .get("--discard-manifest")
        .map(|hash| parse_digest(hash))
        .transpose()?;
    progress("fulltext-rebuild", "reconstruct-missing-search")?;
    let receipt = PostgresOperator::connect_configured(config)?
        .with_maintenance_control(control.clone())
        .rebuild_fulltext(&entry.database_id, expected)?;
    if let Some(stats) = receipt.fulltext {
        println!(
            "FULLTEXT_REBUILT basis_t={} generation={} source_manifest={} records={} blocks={} encoded_bytes={} old_objects_deleted=false",
            receipt.basis_t,
            receipt.generation,
            hex(&receipt.descriptor_id),
            stats.records,
            stats.blocks,
            stats.encoded_bytes
        );
    } else {
        println!("FULLTEXT_REBUILT status=not-required");
    }
    Ok(())
}
