use edb_core::storage::{BlockDatabase, PgBlockStore};
use edb_core::{PortableBackup, PostgresConnectionConfig, Schema};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct RunningChild(Option<Child>);

impl Drop for RunningChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn administrative_signals_cancel_between_batches_and_backup_can_resume() {
    let parameters = std::env::var("EDB_TEST_POSTGRES_URL")
        .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database");
    let config = PostgresConnectionConfig::parse(&parameters).unwrap();
    PgBlockStore::install(&config).unwrap();
    let name = format!(
        "admin-signals-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    BlockDatabase::create(&config, &name, Schema::new()).unwrap();

    for signal in [libc::SIGINT, libc::SIGTERM] {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("backup");
        let command = || {
            let mut command = Command::new(env!("CARGO_BIN_EXE_edb"));
            command
                .args(["backup", "--database", &name, "--repository"])
                .arg(&repository)
                .env("EDB_POSTGRES_URL", &parameters)
                .env_remove("EDB_POSTGRES_TRANSPORT");
            command
        };
        let mut running = RunningChild(Some(
            command()
                .args(["--maintenance-pause-ms", "60000"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ));
        // Observing a copied object establishes that the CLI has installed its
        // handler and started the real backup. Pacing holds a safe work boundary.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let copied = std::fs::read_dir(repository.join("objects"))
                .ok()
                .is_some_and(|mut entries| entries.next().is_some());
            if copied {
                break;
            }
            assert!(
                running.0.as_mut().unwrap().try_wait().unwrap().is_none(),
                "backup exited before copying an object"
            );
            assert!(
                Instant::now() < deadline,
                "backup did not reach its copy phase"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = running.0.as_ref().unwrap().id();
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, signal) }, 0);
        let deadline = Instant::now() + Duration::from_secs(10);
        while running.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "maintenance ignored its cancellation"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let canceled = running.0.take().unwrap().wait_with_output().unwrap();
        assert_eq!(canceled.status.code(), Some(1));
        let diagnostic = String::from_utf8(canceled.stderr).unwrap();
        assert!(diagnostic.contains("category=Interrupted code=maintenance/canceled"));
        assert!(
            !String::from_utf8(canceled.stdout)
                .unwrap()
                .contains("BACKED_UP")
        );

        let resumed = command().output().unwrap();
        assert!(
            resumed.status.success(),
            "{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let points = PortableBackup::list_backup_points(&repository).unwrap();
        assert_eq!(points.len(), 1);
        let point = &points[0];
        let verified = PortableBackup::verify_backup_point(
            &repository,
            point.basis_t,
            point.log_generation,
            true,
        )
        .unwrap();
        assert_eq!(verified.database.basis_t(), point.basis_t);
    }
}
