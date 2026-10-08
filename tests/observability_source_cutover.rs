use edb_core::{
    DB_DOC, Database, EntityRef, Keyword, OperationContext, OperationKind, Schema, TelemetryConfig,
    TelemetryPhase, TelemetryPublisher, TelemetrySnapshot, TxOp, Value,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);

#[test]
fn operation_publication_keeps_named_semantic_work_without_fact_payloads() {
    let (sender, receiver) = mpsc::channel();
    let publisher = TelemetryPublisher::start(
        TelemetryConfig {
            enabled: true,
            ..Default::default()
        },
        move |line| sender.send(line.to_owned()).map_err(std::io::Error::other),
    )
    .unwrap();
    let operation = OperationContext::named(
        OperationKind::Application,
        Keyword::new("catalog", "refresh"),
    )
    .unwrap();
    let child = operation
        .child_named(
            OperationKind::Transaction,
            Keyword::new("catalog", "contribute"),
        )
        .unwrap();
    let database = Database::new(Schema::new()).unwrap();
    let (result, _) = child.measure(|| {
        database.with(
            &[TxOp::Add {
                entity: EntityRef::Id(400),
                attribute: DB_DOC as u32,
                value: Value::String("private-value-should-not-be-published".into()).into(),
            }],
            10,
            EntityRef::Id(99),
        )
    });
    let result = result.unwrap();
    let report = operation.report();
    assert_eq!(result.tx_data.len(), 2);
    assert_eq!(report.stats.transaction.produced_datoms, 2);
    assert!(receiver.try_recv().is_err()); // Capture alone does not call the sink.
    assert!(publisher.emitter().publish_operation(&report));
    let line = receiver.recv_timeout(WAIT).unwrap();
    assert!(line.ends_with('\n'));
    assert_eq!(line.lines().count(), 1);
    assert!(line.contains("\"event\":\"edb.operation\""));
    assert!(line.contains("\"context\":\":catalog/refresh\""));
    assert!(line.contains("\"nested\":[{\"context\":\":catalog/contribute\""));
    // Inclusive parent and named descendant each expose the same actual work.
    assert_eq!(line.matches("\"transaction.produced_datoms\":2").count(), 2);
    assert!(!line.contains("private-value"));
    assert!(!line.contains("tx_data"));
    assert!(receiver.try_recv().is_err());
    assert!(publisher.shutdown(WAIT));
}

#[test]
fn disabled_and_not_due_sampling_never_capture_or_start_sink_work() {
    let sink_called = Arc::new(AtomicBool::new(false));
    let called = sink_called.clone();
    let disabled = TelemetryPublisher::start(TelemetryConfig::default(), move |_| {
        called.store(true, Ordering::Release);
        Ok(())
    })
    .unwrap();
    assert!(
        !disabled
            .emitter()
            .publish_if_due(|| panic!("disabled capture"))
    );
    assert!(
        !disabled
            .emitter()
            .publish_operation(&OperationContext::new(OperationKind::Query).report())
    );
    assert_eq!(disabled.stats(), Default::default());
    assert!(disabled.shutdown(Duration::ZERO));
    assert!(!sink_called.load(Ordering::Acquire));

    let (sender, receiver) = mpsc::channel();
    let publisher = TelemetryPublisher::start(
        TelemetryConfig {
            enabled: true,
            interval: Duration::from_secs(3600),
            ..Default::default()
        },
        move |line| sender.send(line.to_owned()).map_err(std::io::Error::other),
    )
    .unwrap();
    let emitter = publisher.emitter();
    assert!(emitter.publish_if_due(|| TelemetrySnapshot::capture(
        None,
        TelemetryPhase::Waiting,
        false
    )));
    assert!(!emitter.publish_if_due(|| panic!("not due capture")));
    let line = receiver.recv_timeout(WAIT).unwrap();
    assert!(line.contains("\"event\":\"edb.metrics\""));
    assert!(line.contains("\"phase\":\"waiting\""));
    assert!(!line.contains("edb.warning"));
    assert_eq!(emitter.stats().accepted, 1);
    assert!(publisher.shutdown(WAIT));
}

#[test]
fn blocked_sink_preserves_whole_event_admission_and_bounded_shutdown() {
    let (entered, active) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let (completed, done) = mpsc::channel();
    let publisher = TelemetryPublisher::start(
        TelemetryConfig {
            enabled: true,
            queue_capacity: 1,
            max_event_bytes: 4096,
            ..Default::default()
        },
        move |line| {
            assert!(line.starts_with('{') && line.ends_with("}\n"));
            entered.send(()).unwrap();
            blocked.recv_timeout(WAIT).unwrap();
            completed.send(()).unwrap();
            Ok(())
        },
    )
    .unwrap();
    let emitter = publisher.emitter();
    let report = OperationContext::new(OperationKind::Query).report();
    assert!(emitter.publish_operation(&report));
    active.recv_timeout(WAIT).unwrap();
    assert!(emitter.publish_operation(&report)); // One waiting event, one active.
    assert!(!emitter.publish_operation(&report));
    let detailed =
        OperationContext::named(OperationKind::Query, Keyword::new("catalog", "detailed")).unwrap();
    for i in 0..24 {
        detailed
            .child_named(
                OperationKind::Query,
                Keyword::new("catalog", format!("part-{i}")),
            )
            .unwrap()
            .record_payload_read(1);
    }
    assert!(!emitter.publish_operation(&detailed.report()));
    assert_eq!(emitter.stats().accepted, 2);
    assert_eq!(emitter.stats().dropped_full, 1);
    assert_eq!(emitter.stats().dropped_oversize, 1);
    publisher.request_stop();
    assert!(!emitter.publish_operation(&report));
    assert_eq!(emitter.stats().dropped_stopped, 1);
    // The controlled callback cannot finish until release; zero wait detaches it.
    assert!(!publisher.shutdown(Duration::ZERO));
    release.send(()).unwrap();
    done.recv_timeout(WAIT).unwrap();
    // Sender destruction proves worker completion, including pending discard.
    assert!(active.recv_timeout(WAIT).is_err());
    assert_eq!(emitter.stats().delivered, 1);
    assert_eq!(emitter.stats().dropped_shutdown, 1);
    assert_eq!(emitter.stats().sink_errors, 0);
    assert_eq!(emitter.stats().sink_panics, 0);
}
