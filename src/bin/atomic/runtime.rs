//! Stock foreground transactor lifecycle; no administrative provisioning.
use super::{
    Arguments, STOP,
    health::{HealthServer, Phase},
    install_signals, io_error, transaction_defaults, usage,
};
use atomic_core::{
    BackgroundIndexingConfig, CapacityLimits, ErrorCategory, ExcisionConfig,
    LocalTransactionEndpoint, LocalTransportConfig, PostgresConnectionConfig, SemanticError,
    ServiceOptions, TelemetryConfig, TelemetryPhase, TelemetryPublisher, TelemetrySnapshot,
    TransactionExecutionOptions, TransactionService, TransactionServiceConfig, TransactionStandby,
};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub(super) fn run(
    args: &Arguments,
    connection: PostgresConnectionConfig,
) -> Result<(), SemanticError> {
    let mut capacity = CapacityLimits::default();
    capacity.writer_tree_cache_entries =
        args.size("--tree-cache-entries", capacity.writer_tree_cache_entries)?;
    capacity.writer_tree_cache_bytes =
        args.size("--tree-cache-bytes", capacity.writer_tree_cache_bytes)?;
    let config = TransactionServiceConfig {
        connection: connection.clone(),
        database_id: args.required("--database")?.to_owned(),
        holder_id: args
            .options
            .get("--holder")
            .cloned()
            .unwrap_or_else(|| format!("atomic-{}", std::process::id())),
        lease_duration: Duration::from_millis(args.number("--lease-ms", 5_000)?),
        renew_interval: Duration::from_millis(args.number("--renew-ms", 1_000)?),
        queue_capacity: args.size("--queue-capacity", 64)?,
        capacity_limits: capacity,
    };
    let default_index = BackgroundIndexingConfig::default();
    let index = BackgroundIndexingConfig {
        memory_index_threshold_bytes: args.number(
            "--index-threshold-bytes",
            default_index.memory_index_threshold_bytes,
        )?,
        memory_index_max_bytes: args
            .number("--index-max-bytes", default_index.memory_index_max_bytes)?,
    };
    let default_transport = LocalTransportConfig::default();
    let transport = LocalTransportConfig {
        max_in_flight: args.size("--max-in-flight", default_transport.max_in_flight)?,
        request_timeout: Duration::from_millis(args.number("--request-timeout-ms", 30_000)?),
        max_frame_bytes: args.size("--max-frame-bytes", default_transport.max_frame_bytes)?,
    };
    transport.validate()?;
    let local_endpoint = args
        .options
        .get("--endpoint")
        .map(|path| LocalTransactionEndpoint::bind_at(Path::new(path)))
        .transpose()?;
    let remote_endpoint = if let Some(listen) = args.options.get("--listen") {
        let (identity, token) = atomic_core::remote_server_credentials_from_env()?;
        let endpoint = atomic_core::RemoteTransactionEndpoint::bind(
            listen
                .parse()
                .map_err(|_| usage("invalid listen address"))?,
            identity,
        )?;
        let mut advertised: std::net::SocketAddr = args
            .required("--advertise")?
            .parse()
            .map_err(|_| usage("invalid advertised address"))?;
        if advertised.port() == 0 {
            advertised.set_port(endpoint.local_addr()?.port());
        }
        Some((
            endpoint,
            token,
            advertised,
            args.required("--tls-server-name")?.to_owned(),
        ))
    } else {
        None
    };
    install_signals()?;
    let defaults =
        transaction_defaults(args.options.get("--default-partition").map(String::as_str))?;
    let excision = ExcisionConfig::default();
    let telemetry = TelemetryPublisher::stdout(TelemetryConfig {
        enabled: args.options.contains_key("--telemetry-ms"),
        interval: Duration::from_millis(args.number("--telemetry-ms", 60_000)?),
        ..Default::default()
    })?;
    let emitter = telemetry.emitter();
    let options = ServiceOptions {
        hint_prefetch: atomic_core::HintPrefetchConcurrency {
            max_workers: args.size("--hint-workers", 1)?,
        },
        index_preparation_parallelism: args.size("--index-workers", 1)?,
        fulltext_build_limits: Default::default(),
        telemetry: emitter.is_enabled().then(|| emitter.clone()),
        indexing: index,
        execution: TransactionExecutionOptions {
            defaults,
            ..Default::default()
        },
        excision: ExcisionConfig {
            max_admitted_bytes: args.number("--excision-max-bytes", excision.max_admitted_bytes)?,
            log_batch_transactions: args
                .size("--excision-log-batch", excision.log_batch_transactions)?,
            ..excision
        },
    };
    let health = args
        .options
        .get("--health-listen")
        .map(|address| {
            HealthServer::bind(
                address
                    .parse()
                    .map_err(|_| usage("invalid health address"))?,
            )
        })
        .transpose()?;
    if let Some(health) = &health {
        println!(
            "HEALTH address={} plaintext_status_only=true",
            health.address()
        );
        std::io::stdout().flush().map_err(|_| io_error())?;
    }
    let activate = || -> Result<Option<TransactionService>, SemanticError> {
        if args
            .options
            .get("--mode")
            .is_some_and(|mode| mode == "auto")
        {
            let mut standby = TransactionStandby::start_with_options(
                config,
                options,
                Duration::from_millis(args.number("--standby-poll-ms", 250)?),
            )?;
            set_phase(&health, Phase::Standby);
            println!(
                "STANDBY database={:?} waiting_for_authority=true",
                args.required("--database")?
            );
            std::io::stdout().flush().map_err(|_| io_error())?;
            loop {
                emitter.publish_if_due(|| {
                    TelemetrySnapshot::capture(None, TelemetryPhase::Waiting, false)
                });
                if STOP.load(Ordering::Relaxed) {
                    set_phase(&health, Phase::Stopping);
                    standby.shutdown();
                    return Ok(None);
                }
                if health.as_ref().is_some_and(|server| !server.is_available()) {
                    return Err(SemanticError::new(
                        ErrorCategory::Unavailable,
                        "cli/health-listener",
                        "health listener stopped",
                    ));
                }
                if let Some(service) = standby.try_active()? {
                    standby.shutdown();
                    return Ok(Some(service));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        } else {
            TransactionService::start_with_options(config, options).map(Some)
        }
    };
    let service = match activate() {
        Ok(Some(service)) => service,
        Ok(None) => {
            if telemetry.shutdown(Duration::from_millis(100)) {
                println!("STOPPED");
            }
            return Ok(());
        }
        Err(error) => {
            set_phase(&health, Phase::Failed);
            emitter.try_publish(TelemetrySnapshot::capture(
                None,
                TelemetryPhase::Failed,
                false,
            ));
            telemetry.shutdown(Duration::from_millis(100));
            return Err(error);
        }
    };
    if STOP.load(Ordering::Relaxed) {
        set_phase(&health, Phase::Stopping);
        service.shutdown();
        if telemetry.shutdown(Duration::from_millis(100)) {
            println!("STOPPED");
        }
        return Ok(());
    }
    let started = (|| -> Result<_, SemanticError> {
        let local = local_endpoint
            .map(|endpoint| endpoint.start(service.client(), transport))
            .transpose()?;
        let remote = if let Some((endpoint, token, advertised, name)) = remote_endpoint {
            let config = atomic_core::RemoteTransportConfig {
                max_in_flight: transport.max_in_flight,
                request_timeout: transport.request_timeout,
                max_frame_bytes: transport.max_frame_bytes,
                ..Default::default()
            };
            let server = endpoint.start(service.client(), config, token)?;
            server.publish(&connection, advertised, &name)?;
            Some(server)
        } else {
            None
        };
        Ok((local, remote))
    })();
    let (local_server, remote_server) = match started {
        Ok(servers) => servers,
        Err(error) => {
            set_phase(&health, Phase::Failed);
            service.shutdown();
            return Err(error);
        }
    };
    set_phase(&health, Phase::Active);
    if let Some(server) = &local_server {
        println!(
            "READY database={:?} endpoint={:?} lineage={}",
            service.identity().database_id(),
            server.endpoint(),
            service.identity().lineage_id()
        );
    } else {
        println!(
            "READY database={:?} transport=tls authenticated_discovery=true lineage={}",
            service.identity().database_id(),
            service.identity().lineage_id()
        );
    }
    std::io::stdout().flush().map_err(|_| io_error())?;
    let mut lost_authority = false;
    let mut maintenance_failure = None;
    while !STOP.load(Ordering::Relaxed) {
        emitter.publish_if_due(|| {
            TelemetrySnapshot::capture(
                Some(&service.client()),
                TelemetryPhase::Active,
                local_server.as_ref().is_some_and(|s| s.is_available())
                    || remote_server.as_ref().is_some_and(|s| s.is_available()),
            )
        });
        let stats = service.background_indexing_stats();
        let current_failure = stats.excision_failure.as_ref().map(|failure| failure.code);
        if current_failure != maintenance_failure {
            if let Some(failure) = &stats.excision_failure {
                eprintln!(
                    "TRANSACTOR_EXCISION_FAILURE category={:?} code={}",
                    failure.category, failure.code
                );
            } else if maintenance_failure.is_some() {
                eprintln!("TRANSACTOR_EXCISION_RECOVERED");
            }
            maintenance_failure = current_failure;
        }
        if !service.client().is_available()
            || local_server
                .as_ref()
                .is_some_and(|server| !server.is_available())
            || remote_server
                .as_ref()
                .is_some_and(|server| !server.is_available())
            || health.as_ref().is_some_and(|server| !server.is_available())
        {
            // Keep the safe diagnostic code before shutdown consumes
            // the service. Messages/details can contain user data.
            if let Some(failure) = service.background_indexing_stats().last_failure {
                eprintln!(
                    "TRANSACTOR_INDEX_FAILURE category={:?} code={}",
                    failure.category, failure.code
                );
            }
            lost_authority = true;
            set_phase(&health, Phase::Failed);
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if !lost_authority {
        set_phase(&health, Phase::Stopping);
    }
    emitter.try_publish(TelemetrySnapshot::capture(
        Some(&service.client()),
        if lost_authority {
            TelemetryPhase::Failed
        } else {
            TelemetryPhase::Stopping
        },
        false,
    ));
    drop(local_server);
    drop(remote_server);
    service.shutdown();
    let telemetry_stopped = telemetry.shutdown(Duration::from_millis(100));
    if lost_authority {
        return Err(SemanticError::new(
            ErrorCategory::Unavailable,
            "cli/transactor-unavailable",
            "transactor lost availability; supervisor restart required",
        ));
    }
    // A stalled stdout sink may retain the global stdout lock. Do not follow
    // a timed-out sink shutdown with another synchronous write to that lock.
    if telemetry_stopped {
        println!("STOPPED");
    }
    Ok(())
}

fn set_phase(server: &Option<HealthServer>, phase: Phase) {
    if let Some(server) = server {
        server.set(phase);
    }
}
