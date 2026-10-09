//! Process-facing command composition; engine/admin semantics remain in the library.
use super::arguments::HELP;
use super::{Arguments, admin, data, io_error, runtime};
use edb_core::{
    DatabaseCatalog, PostgresOperator, Schema, SemanticError, postgres_config_from_env,
};
use std::io::Write;

pub(super) fn run(args: Arguments) -> Result<(), SemanticError> {
    match args.command.as_str() {
        "--help" | "help" => {
            print!("{HELP}{}{}", admin::HELP, data::HELP);
            return Ok(());
        }
        "--version" => {
            println!("edb {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    let connection = postgres_config_from_env()?;
    match args.command.as_str() {
        "install" => {
            edb_core::storage::PgBlockStore::install(&connection)?;
            println!("INSTALLED storage_format=edb/opaque-storage/2 tables=2");
            std::io::stdout().flush().map_err(|_| io_error())?;
            if let Some(writer) = args.options.get("--writer-role") {
                edb_core::storage::PgBlockStore::connect(&connection)?
                    .grant_runtime_privileges(writer, args.required("--peer-role")?)?;
                println!("GRANTED runtime_roles=true additive=true object_delete=operator_only");
            }
        }
        "create" => {
            let id = args.required("--database")?;
            let result = DatabaseCatalog::connect_configured(&connection)?
                .create_if_absent(id, Schema::new())?;
            println!(
                "{} name={id:?} storage_id={:?} lineage={}",
                if result.created { "CREATED" } else { "EXISTS" },
                result.database.database_id,
                result.database.lineage_id
            );
        }
        "status" => {
            let entry = DatabaseCatalog::connect_configured(&connection)?
                .resolve(args.required("--database")?)?;
            let status = PostgresOperator::connect_configured(&connection)?
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
            admin::consolidate(
                &connection,
                args.required("--database")?,
                &edb_core::MaintenanceControl::default(),
            )?;
        }
        "transactor" => runtime::run(&args, connection)?,
        _ => unreachable!("validated command"),
    }
    Ok(())
}
