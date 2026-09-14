//! Three independent application processes: a writer endpoint and two peers.
//! ATOMIC_POSTGRES_URL='host=... user=... dbname=...' cargo run --example process_workflow
use atomic_core::{
    Attribute, AttributeName, AttributeRef, Cardinality, Connection, EntityMap, EntityRef, Keyword,
    LocalTransactionServer, LocalTransportConfig, MapValue, PostgresConnectionConfig,
    PullAttribute, PullPattern, Schema, TransactionRequest, TransactionService,
    TransactionServiceConfig, TxForm, TxOp, Value, ValueType,
};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct WriterProcess(std::process::Child);
impl std::ops::Deref for WriterProcess {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for WriterProcess {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for WriterProcess {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

const NAME: u32 = 1_000;
const WAIT: Duration = Duration::from_secs(20);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let postgres = std::env::var("ATOMIC_POSTGRES_URL")?;
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("writer") => {
            let writer = TransactionService::start(TransactionServiceConfig {
                connection: PostgresConnectionConfig::parse(&postgres)?,
                database_id: arguments[1].clone(),
                holder_id: format!("process-writer-{}", std::process::id()),
                lease_duration: Duration::from_secs(5),
                renew_interval: Duration::from_millis(100),
                queue_capacity: 16,
                capacity_limits: Default::default(),
            })?;
            let server =
                LocalTransactionServer::start(writer.client(), LocalTransportConfig::default())?;
            println!("{}", server.endpoint().display());
            std::io::stdout().flush()?;
            let mut stop = String::new();
            std::io::stdin().read_line(&mut stop)?;
            drop(server);
            writer.shutdown();
            return Ok(());
        }
        Some("peer") => {
            let peer = Connection::connect(&postgres, &arguments[1], 8)?;
            let name = arguments[3].clone();
            let request = TransactionRequest::from_forms(
                format!("person-{name}"),
                vec![TxForm::EntityMap(EntityMap {
                    id: Some(EntityRef::Temp("person".into())),
                    attributes: vec![(
                        AttributeRef::Ident(Keyword::new("person", "name")),
                        MapValue::Value(Value::String(name.clone()).into()),
                    )],
                })],
            );
            let committed = peer.transact_socket(&arguments[2], request.clone(), WAIT)?;
            let report = committed.report?;
            assert_eq!(
                report.db_after.values(report.tempids["person"], NAME)?,
                vec![Value::String(name.clone())]
            );
            let replay = peer.transact_socket(&arguments[2], request, WAIT)?;
            assert!(replay.replayed);
            assert_eq!(replay.basis_t, committed.basis_t);
            assert_eq!(replay.report?.tempids, report.tempids);
            println!(
                "peer pid={} name={name} committed t={}",
                std::process::id(),
                report.basis_t
            );
            return Ok(());
        }
        None => {}
        _ => {
            return Err(
                "usage: process_workflow [writer DATABASE | peer DATABASE SOCKET NAME]".into(),
            );
        }
    }
    let database = format!(
        "process-workflow-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let storage = PostgresConnectionConfig::plaintext(&postgres);
    atomic_core::storage::PgBlockStore::install(&storage)?;
    atomic_core::storage::BlockDatabase::create(&storage, &database, Schema::new())?;
    let binary = std::env::current_exe()?;
    let mut writer = WriterProcess(
        Command::new(&binary)
            .args(["writer", &database])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let mut endpoint = String::new();
    BufReader::new(writer.stdout.take().ok_or("writer stdout unavailable")?)
        .read_line(&mut endpoint)?;
    if endpoint.trim().is_empty() {
        return Err(format!("writer failed: {:?}", writer.wait()?).into());
    }
    let endpoint = endpoint.trim().to_owned();
    let observer = Connection::connect(&postgres, &database, 8)?;
    let schema = observer
        .transact_socket(
            &endpoint,
            TransactionRequest::new(
                "schema",
                vec![TxOp::InstallAttribute(Attribute::new(
                    NAME,
                    Keyword::new("person", "name"),
                    ValueType::String,
                    Cardinality::One,
                ))],
            ),
            WAIT,
        )?
        .report?;
    observer.sync_to(schema.basis_t, WAIT)?;
    observer.enable_transaction_reports();
    let old = observer.db();
    let first = Command::new(&binary)
        .args(["peer", &database, &endpoint, "Ada"])
        .stdout(Stdio::piped())
        .spawn()?;
    let second = Command::new(&binary)
        .args(["peer", &database, &endpoint, "Grace"])
        .stdout(Stdio::piped())
        .spawn()?;
    for peer in [first, second] {
        let output = peer.wait_with_output()?;
        assert!(output.status.success(), "independent peer failed");
        print!("{}", String::from_utf8(output.stdout)?);
    }
    let current = observer.sync_to(schema.basis_t + 2, WAIT)?;
    let pattern = PullPattern::attributes(vec![PullAttribute::forward(AttributeName::Id(NAME))]);
    for expected in schema.basis_t + 1..=schema.basis_t + 2 {
        let report = observer
            .next_transaction_report(WAIT)?
            .ok_or("missing remote transaction report")?;
        assert_eq!(report.basis_t, expected);
        let name = report.db_after.values(report.tempids["person"], NAME)?[0].clone();
        assert_eq!(
            current.pull(&pattern, report.tempids["person"])?,
            atomic_core::QueryValue::Map(vec![(
                atomic_core::QueryValue::Scalar(Value::Keyword(Keyword::new("person", "name"))),
                atomic_core::QueryValue::Scalar(name)
            ),])
        );
        assert!(old.values(report.tempids["person"], NAME)?.is_empty());
    }
    assert!(observer.try_next_transaction_report().is_none());
    writer
        .stdin
        .take()
        .ok_or("writer stdin unavailable")?
        .write_all(b"stop\n")?;
    assert!(writer.wait()?.success());
    let reopened = Connection::connect(&postgres, &database, 8)?;
    assert_eq!(reopened.db().basis_t(), current.basis_t());
    assert_eq!(reopened.identity(), observer.identity());
    println!(
        "PASS {database}: independent writer/peers, schema, concurrent maps, retries, complete reports, native pull, immutable values, writer-offline reopen"
    );
    Ok(())
}
