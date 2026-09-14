//! A compiled application transactor, not a dynamic code loader.
//! Provision the database and :person/name first; see docs/07_peer_api/02_native_computation.md.
use atomic_core::*;
use std::io::Write;
use std::time::Duration;

fn registry() -> Result<NativeRegistry, SemanticError> {
    let mut builder = NativeRegistry::builder();
    builder.transaction(
        Symbol::new("demo.people.v1", "add-person"),
        |db, args, control| {
            let [
                RuntimeValue::Scalar(Value::String(tempid)),
                RuntimeValue::Scalar(Value::String(name)),
            ] = args
            else {
                return Err(SemanticError::incorrect(
                    "demo/arguments",
                    "add-person requires a tempid and name string",
                ));
            };
            if db.entid(&Keyword::new("person", "name")).is_none() {
                return Err(SemanticError::incorrect(
                    "demo/schema",
                    "install :person/name first",
                ));
            }
            // UTF-8 lowercasing is ordinary Rust application code. It is not a VM
            // instruction or a serialized closure. Admit worst-case output first.
            control.reserve(name.len().saturating_mul(4).saturating_add(tempid.len()))?;
            let mut normalized = String::new();
            for word in name.split_whitespace() {
                if !normalized.is_empty() {
                    normalized.push(' ');
                }
                for letter in word.chars() {
                    control.check(1)?;
                    normalized.extend(letter.to_lowercase());
                }
            }
            if normalized.is_empty() {
                return Err(SemanticError::incorrect(
                    "demo/name",
                    "name must contain non-whitespace characters",
                ));
            }
            Ok(vec![TxForm::EntityMap(EntityMap {
                id: Some(EntityRef::Temp(tempid.clone())),
                attributes: vec![(
                    AttributeRef::Ident(Keyword::new("person", "name")),
                    MapValue::Value(Value::String(normalized).into()),
                )],
            })])
        },
    )?;
    Ok(builder.build())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if !(1..=2).contains(&arguments.len()) {
        return Err(
            "usage: native_transaction_host DATABASE [SOCKET] (newline/EOF stops the host)".into(),
        );
    }
    let database = &arguments[0];
    let service = TransactionService::start_with_indexing_and_execution_options(
        TransactionServiceConfig {
            connection: postgres_config_from_env()?,
            database_id: database.clone(),
            holder_id: format!("native-host-{}", std::process::id()),
            lease_duration: Duration::from_secs(5),
            renew_interval: Duration::from_millis(500),
            queue_capacity: 32,
            capacity_limits: Default::default(),
        },
        BackgroundIndexingConfig::default(),
        TransactionExecutionOptions {
            native: registry()?,
            ..Default::default()
        },
    )?;
    let server = if let Some(endpoint) = arguments.get(1) {
        LocalTransactionServer::start_at(
            service.client(),
            LocalTransportConfig::default(),
            endpoint,
        )?
    } else {
        LocalTransactionServer::start(service.client(), LocalTransportConfig::default())?
    };
    println!("READY endpoint={}", server.endpoint().display());
    std::io::stdout().flush()?;
    let mut stop = String::new();
    std::io::stdin().read_line(&mut stop)?;
    drop(server);
    service.shutdown();
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        if let Some(error) = error.downcast_ref::<SemanticError>() {
            eprintln!("native host failed: {:?} {}", error.category, error.code);
        } else {
            eprintln!("native host failed; verify arguments and explicit PostgreSQL configuration");
        }
        std::process::exit(1);
    }
}
