use edb_core::{EntityRef, TransactionRequest, submission_request_digest};

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    text.split_once(start)
        .expect("documented example start")
        .1
        .split_once(end)
        .expect("documented example end")
        .0
}

#[test]
fn connect_tutorial_rust_and_cli_examples_have_one_exact_request_identity() {
    let page = include_str!("../docs/01_tutorials/05_connect_to_a_database.md");
    let rust_key = between(page, "TransactionRequest::from_edn(\n    \"", "\"");
    let rust_data = between(page, "r#\"", "\"#");
    let rust_source = between(page, "EntityRef::from_edn(\"", "\")?");
    let rust_request = TransactionRequest::from_edn(
        rust_key,
        rust_data,
        EntityRef::from_edn(rust_source).unwrap(),
    )
    .unwrap();

    let cli_key = between(page, "--request-key ", " ");
    let cli_source = between(page, "--source '", "'");
    let cli_data = between(page, "<<'EDN'\n", "\nEDN");
    // Match the CLI's schema-independent construction, without opening a peer
    // or resolving the ident before authoritative receipt lookup.
    let cli_request =
        TransactionRequest::from_edn(cli_key, cli_data, EntityRef::from_edn(cli_source).unwrap())
            .unwrap();
    let digest = |request: &TransactionRequest| {
        submission_request_digest(
            &request.forms,
            &request.source,
            request.compare_basis_t,
            request.tx_instant_override,
        )
        .unwrap()
    };
    assert_eq!(rust_request.request_key, cli_request.request_key);
    assert_eq!(digest(&rust_request), digest(&cli_request));
}
