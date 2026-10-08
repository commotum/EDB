use edb_core::edn::{EdnValue, read_edn};
use edb_core::edn_value::edn_keyword;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn field<'a>(value: &'a EdnValue, namespace: &str, name: &str) -> &'a EdnValue {
    let EdnValue::Map(fields) = value else {
        panic!("expected map: {value:?}")
    };
    &fields
        .iter()
        .find(|(key, _)| key == &edn_keyword(namespace, name))
        .unwrap_or_else(|| panic!("missing :{namespace}/{name} in {value:?}"))
        .1
}

fn integer(value: &EdnValue) -> u64 {
    if let EdnValue::Tagged(tag, value) = value {
        assert_eq!(tag.qualified_name(), "edb/ref");
        return integer(value);
    }
    let EdnValue::Long(value) = value else {
        panic!("expected integer: {value:?}")
    };
    u64::try_from(*value).unwrap()
}

struct Cli {
    postgres: String,
    database: String,
    directory: tempfile::TempDir,
}

impl Cli {
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_edb"));
        command
            .env("EDB_POSTGRES_URL", &self.postgres)
            .env_remove("EDB_POSTGRES_TRANSPORT");
        command
    }

    fn run(&self, args: &[&str], input: Option<&str>) -> Output {
        let mut command = self.command();
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command.spawn().unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str], input: Option<&str>) -> String {
        let output = self.run(args, input);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn edn(&self, args: &[&str], input: Option<&str>) -> EdnValue {
        read_edn(&self.ok(args, input)).unwrap()
    }

    fn file(&self, name: &str, contents: &str) -> String {
        let path = self.directory.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path.to_str().unwrap().into()
    }

    fn endpoint(&self) -> String {
        self.directory
            .path()
            .join("writer.sock")
            .to_str()
            .unwrap()
            .into()
    }

    fn writer(&self) -> Writer {
        let stdout = self.directory.path().join("writer.out");
        let mut child = self
            .command()
            .args([
                "transactor",
                "--database",
                &self.database,
                "--endpoint",
                &self.endpoint(),
            ])
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(std::fs::File::create(self.directory.path().join("writer.err")).unwrap())
            .spawn()
            .unwrap();
        let started = Instant::now();
        loop {
            if std::fs::read_to_string(&stdout).unwrap().contains("READY ") {
                break;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "writer exited: {}",
                std::fs::read_to_string(self.directory.path().join("writer.err")).unwrap()
            );
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "writer startup timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Writer(child)
    }

    fn transact(&self, key: &str, source: &str, contents: &str) -> EdnValue {
        self.edn(
            &[
                "transact",
                "--database",
                &self.database,
                "--endpoint",
                &self.endpoint(),
                "--request-key",
                key,
                "--source",
                source,
                "--file",
                "-",
            ],
            Some(contents),
        )
    }

    fn query(&self, text: &str, options: &[&str]) -> EdnValue {
        let mut args = vec!["query", "--database", &self.database, "--file", "-"];
        args.extend(options);
        self.edn(&args, Some(text))
    }

    fn pull(&self, options: &[&str]) -> EdnValue {
        self.pull_entity("[:person/email \"alice@example.com\"]", options)
    }

    fn pull_entity(&self, entity: &str, options: &[&str]) -> EdnValue {
        let mut args = vec![
            "pull",
            "--database",
            &self.database,
            "--file",
            "examples/edn/person-pull.edn",
            "--entity",
            entity,
        ];
        args.extend(options);
        self.edn(&args, None)
    }
}

struct Writer(Child);
impl Drop for Writer {
    fn drop(&mut self) {
        // Exercise the supported graceful stop so the same endpoint can restart.
        unsafe {
            libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
        }
        let started = Instant::now();
        while self.0.try_wait().unwrap().is_none() {
            if started.elapsed() > Duration::from_secs(20) {
                self.0.kill().unwrap();
                self.0.wait().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn report(value: &EdnValue, source: u64, committed: bool) -> u64 {
    assert_eq!(field(value, "edb", "committed"), &EdnValue::Bool(committed));
    let basis = integer(field(value, "edb", "db-after-t"));
    let tx = edb_core::t_to_tx(basis).unwrap();
    let EdnValue::Vector(rows) = field(value, "edb", "tx-data") else {
        panic!("expected datoms")
    };
    let mut timestamps = 0;
    for row in rows {
        let EdnValue::Vector(slots) = row else {
            panic!("expected datom vector")
        };
        assert_eq!(slots.len(), 6);
        assert_eq!(integer(&slots[3]), tx);
        assert_eq!(integer(&slots[4]), source);
        assert!(matches!(slots[5], EdnValue::Bool(_)));
        if integer(&slots[1]) == edb_core::DB_TX_INSTANT {
            assert_eq!(integer(&slots[0]), tx);
            assert_eq!(slots[5], EdnValue::Bool(true));
            timestamps += 1;
        }
    }
    assert_eq!(timestamps, 1);
    basis
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn tutorial_cli_preserves_sources_previews_views_and_exact_restart_retries() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let cli = Cli {
        postgres: std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
        database: format!(
            "edn-workflow-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ),
        directory,
    };
    assert!(Path::new("examples/edn/schema.edn").exists());
    cli.ok(&["install"], None);
    cli.ok(&["create", "--database", &cli.database], None);
    let writer = cli.writer();
    let system = ":db.source/system";
    let schema = cli.edn(
        &[
            "transact",
            "--database",
            &cli.database,
            "--endpoint",
            &cli.endpoint(),
            "--request-key",
            "install-person-schema",
            "--source",
            system,
            "--file",
            "examples/edn/schema.edn",
        ],
        None,
    );
    report(&schema, edb_core::DB_SOURCE_SYSTEM, true);
    let people = cli.edn(
        &[
            "transact",
            "--database",
            &cli.database,
            "--endpoint",
            &cli.endpoint(),
            "--request-key",
            "create-alice",
            "--source",
            system,
            "--file",
            "examples/edn/people.edn",
        ],
        None,
    );
    let people_basis = report(&people, edb_core::DB_SOURCE_SYSTEM, true);
    let names = cli.edn(
        &[
            "query",
            "--database",
            &cli.database,
            "--file",
            "examples/edn/names-query.edn",
        ],
        None,
    );
    let EdnValue::Set(rows) = names else {
        panic!("expected query relation")
    };
    assert_eq!(rows.len(), 2);
    let initial_pull = cli.pull(&[]);
    assert_eq!(
        field(&initial_pull, "person", "name"),
        &EdnValue::String("Alice".into())
    );
    assert_eq!(
        field(&initial_pull, "person", "email"),
        &EdnValue::String("alice@example.com".into())
    );
    let alice_id = integer(&cli.query(
        "[:find ?e . :where [?e :person/email \"alice@example.com\"]]",
        &[],
    ))
    .to_string();

    let update = r#"[{:db/id [:person/email "alice@example.com"] :person/name "Alicia"}]"#;
    let preview = cli.edn(
        &[
            "with",
            "--database",
            &cli.database,
            "--source",
            system,
            "--file",
            "-",
        ],
        Some(update),
    );
    report(&preview, edb_core::DB_SOURCE_SYSTEM, false);
    let instant = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    let explicit = format!("[{{:db/id \"edb.tx\" :db/txInstant #edb/instant-millis {instant}}}]");
    let preview_time = cli.edn(
        &[
            "with",
            "--database",
            &cli.database,
            "--source",
            system,
            "--tx-instant",
            &instant,
            "--file",
            "-",
        ],
        Some(&explicit),
    );
    report(&preview_time, edb_core::DB_SOURCE_SYSTEM, false);
    assert_eq!(cli.pull(&[]), initial_pull);
    let changed = cli.transact("rename-alice", system, update);
    let changed_basis = report(&changed, edb_core::DB_SOURCE_SYSTEM, true);
    let redundant = cli.transact(
        "primitive-alicia",
        system,
        r#"[[:db/add [:person/email "alice@example.com"] :person/name "Alicia"]]"#,
    );
    report(&redundant, edb_core::DB_SOURCE_SYSTEM, true);
    let empty = cli.transact("empty", system, "[]");
    report(&empty, edb_core::DB_SOURCE_SYSTEM, true);
    for receipt in [&redundant, &empty] {
        let EdnValue::Vector(rows) = field(receipt, "edb", "tx-data") else {
            panic!("expected datoms")
        };
        assert_eq!(rows.len(), 1);
    }
    let nested = cli.transact("rename-bob", system, r#"[{:db/id [:person/email "alice@example.com"] :person/friends [{:db/id [:person/email "bob@example.com"] :person/name "Robert"}]}]"#);
    report(&nested, edb_core::DB_SOURCE_SYSTEM, true);
    assert_eq!(
        field(
            &cli.pull(&["--as-of", &people_basis.to_string()]),
            "person",
            "name"
        ),
        &EdnValue::String("Alice".into())
    );
    assert_eq!(
        field(
            &cli.pull_entity(&alice_id, &["--since", &people_basis.to_string()]),
            "person",
            "name"
        ),
        &EdnValue::String("Alicia".into())
    );
    assert_eq!(
        cli.query(
            "[:find ?name :where [?e :person/name ?name]]",
            &["--as-of", &people_basis.to_string()]
        ),
        read_edn(r#"#{["Alice"] ["Bob"]}"#).unwrap()
    );
    assert_eq!(
        cli.query(
            "[:find ?added :where [?e :person/name \"Alice\" _ _ ?added]]",
            &["--history"]
        ),
        read_edn("#{[true] [false]}").unwrap()
    );
    assert_eq!(
        cli.query(
            "[:find ?name :where [?e :person/name ?name]]",
            &["--since", &changed_basis.to_string()]
        ),
        read_edn(r#"#{["Robert"]}"#).unwrap()
    );
    let inputs = cli.file("inputs.edn", r#"["alice@example.com"]"#);
    assert_eq!(
        cli.query(
            "[:find ?name :in $ ?email :where [?e :person/email ?email] [?e :person/name ?name]]",
            &["--inputs", &inputs]
        ),
        read_edn(r#"#{["Alicia"]}"#).unwrap()
    );
    let sources = cli.file("sources.edn", &format!("{{$then {{:database {:?} :as-of {people_basis}}} $now {{:database {:?}}} $log {{:database {:?} :log true}} $rows [[\"alice@example.com\" \"external information\"]]}}", cli.database, cli.database, cli.database));
    assert_eq!(cli.query("[:find ?old ?new ?info :in $then $now $log $rows :where [$then ?e :person/name ?old] [$now ?e :person/name ?new] [$now ?e :person/email ?email] [$rows ?email ?info] [(tx-data $log 2) [[_ _ _ _ _ _]]]]", &["--sources", &sources]), read_edn(r#"#{["Alice" "Alicia" "external information"]}"#).unwrap());

    let source_created = cli.transact(
        "describe-source",
        "\"curator\"",
        r#"[{:db/id "curator" :db/ident :source/curator :person/name "Curator"}]"#,
    );
    let EdnValue::Map(ids) = field(&source_created, "edb", "tempids") else {
        panic!("expected tempids")
    };
    let source_id = integer(
        &ids.iter()
            .find(|(key, _)| key == &EdnValue::String("curator".into()))
            .unwrap()
            .1,
    );
    report(&source_created, source_id, true);
    let corroboration = cli.transact("corroborate", ":source/curator", update);
    report(&corroboration, source_id, true);
    assert_eq!(
        cli.query("[:find ?s :where [?e :person/name \"Alicia\" _ ?s]]", &[]),
        read_edn(&format!(
            "#{{[#edb/ref {}] [#edb/ref {source_id}]}}",
            edb_core::DB_SOURCE_SYSTEM
        ))
        .unwrap()
    );
    assert_eq!(
        field(&cli.pull(&[]), "person", "name"),
        &EdnValue::String("Alicia".into())
    );
    let withdrawn = cli.transact("withdraw-name", system, r#"[[:db/retract [:person/email "alice@example.com"] :person/name "Alicia"] [:db/retract :source/curator :db/ident :source/curator]]"#);
    let final_basis = report(&withdrawn, edb_core::DB_SOURCE_SYSTEM, true);
    assert_eq!(
        cli.query(
            &format!("[:find ?added :where [?e :person/name \"Alicia\" _ {source_id} ?added]]"),
            &["--history"]
        ),
        read_edn("#{[true]}").unwrap()
    );
    assert_eq!(
        cli.query(
            "[:find ?e :where [?e :person/name \"Alicia\" _ :source/curator]]",
            &[]
        ),
        read_edn("#{}").unwrap()
    );
    drop(writer);
    // The tutorial's reads work without a writer process.
    assert_eq!(
        cli.query("[:find ?name :where [?e :person/name ?name]]", &[]),
        read_edn(r#"#{["Robert"] ["Curator"]}"#).unwrap()
    );
    let _writer = cli.writer();
    let reordered_people = r#";; same EDN intent, rearranged map keys
    [{:person/friends [{:person/email "bob@example.com" :person/name "Bob"}]
      :person/aliases ["Al" "A"] :person/email "alice@example.com" :person/name "Alice"}]"#;
    for (key, source, data, original) in [
        ("create-alice", system, reordered_people, &people),
        ("corroborate", ":source/curator", update, &corroboration),
    ] {
        let replay = cli.transact(key, source, data);
        assert_eq!(field(&replay, "edb", "replayed"), &EdnValue::Bool(true));
        for name in [
            "basis-t",
            "tx-hash",
            "db-before-t",
            "db-after-t",
            "tx-data",
            "tempids",
        ] {
            assert_eq!(field(&replay, "edb", name), field(original, "edb", name));
        }
    }
    let conflict = cli.run(
        &[
            "transact",
            "--database",
            &cli.database,
            "--endpoint",
            &cli.endpoint(),
            "--request-key",
            "corroborate",
            "--source",
            system,
            "--file",
            "-",
        ],
        Some(update),
    );
    assert!(!conflict.status.success());
    assert!(
        String::from_utf8_lossy(&conflict.stderr).contains("category=Conflict"),
        "{}",
        String::from_utf8_lossy(&conflict.stderr)
    );
    for (key, data) in [
        ("retired-timestamp", "[[:db/txInstant :db.source/system]]"),
        (
            "operation-source",
            r#"[[:db/add [:person/email "alice@example.com"] :person/name "Alicia" :db.source/system]]"#,
        ),
        (
            "map-source",
            r#"[{:db/id [:person/email "alice@example.com"] :person/name "Alicia" :db/source :db.source/system}]"#,
        ),
    ] {
        let rejected = cli.run(
            &[
                "transact",
                "--database",
                &cli.database,
                "--endpoint",
                &cli.endpoint(),
                "--request-key",
                key,
                "--source",
                system,
                "--file",
                "-",
            ],
            Some(data),
        );
        assert!(!rejected.status.success(), "retired syntax accepted: {key}");
    }
    assert!(
        cli.ok(&["status", "--database", &cli.database], None)
            .contains(&format!("basis_t={final_basis} "))
    );
}

#[test]
fn tutorial_native_library_example_runs_against_a_speculative_database() {
    use edb_core::edn::write_edn;
    use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
    use edb_core::{Database, EntityRef, Keyword, TransactionRequest};

    let source = EntityRef::Ident(Keyword::new("db.source", "system"));
    let data = read_edn(r#"[{:person/name "Alice"}]"#).unwrap();
    let text = write_edn(&data).unwrap();
    let request = TransactionRequest::from_edn("create-alice", &text, source.clone()).unwrap();
    assert_eq!(request.request_key, "create-alice");
    let empty = Database::bootstrap().unwrap();
    let installed = empty
        .with_edn(
            include_str!("../examples/edn/schema.edn"),
            10,
            source.clone(),
        )
        .unwrap();
    let preview = installed.db_after.with_edn(&text, 20, source).unwrap();
    assert!(
        preview
            .tx_data
            .iter()
            .all(|d| d.source == edb_core::DB_SOURCE_SYSTEM)
    );
    let query = parse_query_edn("[:find ?name :where [?e :person/name ?name]]").unwrap();
    let bound = query
        .bind(&[EdnQueryArgument::Source(
            edb_core::QuerySourceValue::Database(preview.db_after.database_value()),
        )])
        .unwrap();
    let result = bound
        .execute(&edb_core::QueryControl::default(), None)
        .unwrap();
    let text_result = write_edn(&bound.result_to_edn(&result.result).unwrap()).unwrap();
    assert_eq!(
        read_edn(&text_result).unwrap(),
        read_edn(r#"#{["Alice"]}"#).unwrap()
    );
    assert_eq!(installed.db_after.basis_t(), 1);
}
