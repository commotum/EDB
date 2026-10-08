use edb_core::PostgresConnectionConfig;
use edb_core::storage::PgBlockStore;
use std::process::Command;

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn cli_install_reports_the_installed_storage_format_on_repeated_installation() {
    let parameters = std::env::var("EDB_TEST_POSTGRES_URL")
        .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database");
    let config = PostgresConnectionConfig::parse(&parameters).unwrap();
    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_edb"))
            .arg("install")
            .env("EDB_POSTGRES_URL", &parameters)
            .env_remove("EDB_POSTGRES_TRANSPORT")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let installed = PgBlockStore::connect(&config)
            .unwrap()
            .read_ref("system/format")
            .unwrap()
            .unwrap()
            .value
            .unwrap();
        let marker = String::from_utf8(installed).unwrap();
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("INSTALLED storage_format={marker} tables=2\n")
        );
    }
}
