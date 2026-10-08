use edb_core::Keyword;
use edb_core::edn::{EdnLimits, EdnReadOptions, EdnReader, EdnValue, read_edn, write_edn};

#[test]
fn constructed_data_is_validated_before_readable_writing() {
    let items = vec![EdnValue::Long(1), EdnValue::String("two".into())];
    let list = EdnValue::List(items.clone());
    let vector = EdnValue::Vector(items);
    let map = EdnValue::Map(vec![
        (
            EdnValue::Keyword(Keyword::new("person", "name")),
            EdnValue::String("Anna".into()),
        ),
        (vector.clone(), EdnValue::Long(2)),
    ]);
    assert_eq!(write_edn(&list).unwrap(), r#"(1 "two")"#);
    assert_eq!(write_edn(&vector).unwrap(), r#"[1 "two"]"#);
    assert_eq!(
        write_edn(&map).unwrap(),
        r#"{:person/name "Anna" [1 "two"] 2}"#
    );
    assert_eq!(read_edn(&write_edn(&map).unwrap()).unwrap(), map);

    // Enum construction permits these shapes; writing applies EDN equality.
    let duplicate_map = EdnValue::Map(vec![
        (list.clone(), EdnValue::Long(1)),
        (vector.clone(), EdnValue::Long(2)),
    ]);
    assert_eq!(
        write_edn(&duplicate_map).unwrap_err().code,
        "edn/duplicate-key"
    );
    let duplicate_set = EdnValue::Set(vec![list, vector]);
    assert_eq!(
        write_edn(&duplicate_set).unwrap_err().code,
        "edn/duplicate-element"
    );
    let distinct_numeric_keys = EdnValue::Map(vec![
        (EdnValue::Long(1), EdnValue::Bool(true)),
        (EdnValue::BigInt(1.into()), EdnValue::Bool(false)),
    ]);
    assert_eq!(
        write_edn(&distinct_numeric_keys).unwrap(),
        "{1 true 1N false}"
    );

    assert_eq!(
        write_edn(&EdnValue::Keyword(Keyword::new("", "find")))
            .unwrap_err()
            .code,
        "edn/keyword"
    );
    assert_eq!(
        write_edn(&EdnValue::Keyword(Keyword::unqualified("find"))).unwrap(),
        ":find"
    );
}

#[test]
fn stream_admission_is_cumulative_and_depth_remains_bounded() {
    let options = EdnReadOptions {
        limits: EdnLimits {
            max_nodes: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut stream = EdnReader::with_options("1 2 3", &options).unwrap();
    assert_eq!(stream.next().unwrap().unwrap(), EdnValue::Long(1));
    assert_eq!(stream.next().unwrap().unwrap(), EdnValue::Long(2));
    assert_eq!(stream.next().unwrap().unwrap_err().code, "edn/capacity");
    assert!(stream.next().is_none());

    let mut discards = EdnReader::with_options("1 #_ 2 3", &options).unwrap();
    assert_eq!(discards.next().unwrap().unwrap(), EdnValue::Long(1));
    assert_eq!(discards.next().unwrap().unwrap_err().code, "edn/capacity");
    assert!(discards.next().is_none());

    let options = EdnReadOptions {
        limits: EdnLimits {
            max_work: 32,
            ..Default::default()
        },
        ..Default::default()
    };
    let commented = format!(";{}\n1", "x".repeat(33));
    let mut comments = EdnReader::with_options(&commented, &options).unwrap();
    assert_eq!(comments.next().unwrap().unwrap_err().code, "edn/capacity");
    assert!(comments.next().is_none());

    let options = EdnReadOptions {
        limits: EdnLimits {
            max_depth: usize::MAX,
            ..Default::default()
        },
        ..Default::default()
    };
    for depth in [128, 129] {
        let text = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        let mut reader = EdnReader::with_options(&text, &options).unwrap();
        let result = reader.next().unwrap();
        if depth == 128 {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err().code, "edn/capacity");
        }
        assert!(reader.next().is_none());
    }

    let mut unicode = EdnReader::new("\"é\" 42").unwrap();
    assert_eq!(
        unicode.next().unwrap().unwrap(),
        EdnValue::String("é".into())
    );
    assert_eq!(unicode.byte_offset(), 4);
    assert_eq!(unicode.next().unwrap().unwrap(), EdnValue::Long(42));
    assert_eq!(unicode.byte_offset(), 7);
    assert!(unicode.next().is_none());
    assert!(EdnReader::new("; comment\n#_ 1").unwrap().next().is_none());
    assert!(read_edn("; comment\n#_ 1").is_err());
}
