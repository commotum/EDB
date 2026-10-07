# Data utilities

Utilities for creating and using data structures. EDB uses ordinary Rust
collections, `Keyword` and `Symbol` fields, and functions in `edb_core::edn`.
See [Programming with Data and EDN](../../02_core_concepts/02_programming_with_data_and_edn.md)
for the notation and [the EDN workflow](../../01_tutorials/00_edn_workflow.md)
for complete examples.

## Utility summary

| Operation | Rust API or representation | Description |
| --- | --- | --- |
| Name | `keyword.name`, `symbol.name` | The name part of a keyword or symbol, as a `String`. |
| Namespace | `keyword.namespace`, `symbol.namespace` | The namespace part, as an `Option<String>`. |
| List | `EdnValue::List(Vec<EdnValue>)` | Creates an EDN list from its items. |
| Vector | `EdnValue::Vector(Vec<EdnValue>)` | Creates an EDN vector from its items. |
| Map | `EdnValue::Map(Vec<(EdnValue, EdnValue)>)` | Creates an EDN map from key/value pairs. |
| Read | `read_edn(&str)` | Reads exactly one item from the source, returning `Result<EdnValue, SemanticError>`. |
| Read all | `EdnReader::new(&str)?.collect::<Result<Vec<_>, _>>()` | Reads all the data in the source, returning a vector or the first error. |
| Iterate | `items.iter()` or `items.into_iter()` | Borrows or consumes an ordinary Rust collection. |
| Write | `write_edn(&EdnValue)` | Returns readable EDN as `Result<String, SemanticError>`. |

## Name and namespace

The `name` field contains the name part of a keyword or symbol. The `namespace`
field contains its namespace, or `None` for an unqualified name. Borrow these
fields with `as_str()` and `as_deref()` when an owned copy is unnecessary.

```rust
use edb_core::{Keyword, Symbol};

let keyword = Keyword::new("person", "name");
assert_eq!(keyword.name.as_str(), "name");
assert_eq!(keyword.namespace.as_deref(), Some("person"));
assert_eq!(keyword.qualified_name(), "person/name");

let symbol = Symbol::unqualified("?person");
assert_eq!(symbol.name.as_str(), "?person");
assert_eq!(symbol.namespace.as_deref(), None);
```

Both types provide `new(namespace, name)`, `unqualified(name)`, and
`qualified_name()`. The qualified name has no leading colon, even for a
keyword. Use `Keyword::unqualified("find")` for `:find`; an empty namespace
passed to `new` is not an absent namespace. Constructors assign the fields;
EDN reading and writing validate identifier syntax.

## Lists, vectors, and maps

Create a list or vector from the items to include. Create a map from pairs to
include, written as `(key1, value1), (key2, value2)`, and so on:

```rust
use edb_core::Keyword;
use edb_core::edn::{EdnValue, read_edn, write_edn};

let items = vec![EdnValue::Long(1), EdnValue::String("two".into())];
let list = EdnValue::List(items.clone());
let vector = EdnValue::Vector(items);
let map = EdnValue::Map(vec![
    (EdnValue::Keyword(Keyword::new("person", "name")),
     EdnValue::String("Anna".into())),
]);

assert_eq!(write_edn(&list)?, r#"(1 "two")"#);
assert_eq!(write_edn(&vector)?, r#"[1 "two"]"#);
assert_eq!(map, read_edn(r#"{:person/name "Anna"}"#)?);
```

These are owned Rust values. A binding without `mut` prevents mutation through
that binding; the collection types themselves are not unmodifiable or persistent
collections. List and vector elements may have different EDN types. Map keys and
values may be any EDN values, including collections. `EdnValue::Set` also contains
a `Vec<EdnValue>`.

The reader rejects an odd number of map forms, duplicate map keys, and duplicate
set elements. Direct enum construction does not perform these checks;
`write_edn` validates constructed maps and sets before printing. Lists and vectors
compare as sequences, while maps and sets compare independently of entry order.
Numeric domains remain distinct for EDN equality: `1` and `1N` are distinct keys.

Choose the data structure for the API receiving it. An `EntityMap` has
`id: Option<EntityRef>`, `source: Option<EntityRef>`, and
`attributes: Vec<(AttributeRef, MapValue)>`; `TxForm::EntityMap` describes entity
assertions, not a stored map value. Top-level assertion maps supply a source;
nested maps inherit it unless overridden.
`MapValue::Many` describes multiple assertions and `MapValue::Nested` a nested
entity. Transaction-function arguments use `RuntimeValue::Vector` and
`RuntimeValue::map`; the latter accepts `(Value, RuntimeValue)` pairs, validates
stored-value keys, sorts them, and rejects duplicates. Query data uses
`QueryValue`, whose map keys and values can themselves be query values.
See [Transaction Data](../../04_transactions/04_transaction_data.md) and
[Native Computation](../02_native_computation.md).

## Read

`read_edn(source)` reads one item from the UTF-8 source, returning it. It requires
exactly one value: empty input and additional trailing forms are errors.
Whitespace, commas, comments, and discarded forms do not count as returned values.

```rust
use edb_core::edn::{EdnValue, read_edn};

assert_eq!(read_edn("42 ; a comment\n")?, EdnValue::Long(42));
assert!(read_edn("42 43").is_err());
```

Reading is data-only. Symbols and lists never execute. Unknown tags are preserved
as `EdnValue::Tagged` by default; `#inst` and `#uuid` payloads are validated.
`read_edn_with_options(source, &options)` accepts `EdnReadOptions` to change
limits, reject unknown tags, or register explicit trusted Rust tag handlers.
Clojure evaluation, quoting, auto-resolved keywords, and namespace-map literals
are not supported reader syntax.

## Read all and iterate

`EdnReader` reads all the data in a source one item at a time. Collect its results
to obtain a vector of the parsed data:

```rust
use edb_core::edn::{EdnReader, EdnValue};

let source = "42 :person/name #_ :ignored [1 2]";
let values = EdnReader::new(source)?.collect::<Result<Vec<_>, _>>()?;
assert_eq!(values.len(), 3);
let integers: Vec<i64> = values.iter().filter_map(|value| match value {
    EdnValue::Long(number) => Some(*number),
    _ => None,
}).collect();
assert_eq!(integers, vec![42]);
```

The reader borrows an already available `&str`; it does not accept, own, or close
an I/O reader. The caller manages file reading and file lifetime. `new` and
`with_options` return a `Result`; each iterator item is also a `Result`.
An empty stream produces no items. After an error the iterator produces no more
items. `byte_offset()` reports the current byte position.

Ordinary Rust iterator adapters such as `map`, `filter`, and `collect` operate
on `.iter()` for borrowed items or `.into_iter()` for owned items. `EdnReader`
itself is an iterator, so forms can be processed without collecting all parsed
values first; the full source text remains in memory.

## Writing and native conversion

`write_edn` prints an `EdnValue`, retaining collection order. It is a readable
format, not a canonical durable encoding. `write_edn_with_limits` accepts explicit
limits. Reading and writing default to 16 MiB input/output, 16 MiB tokens, 64 KiB
numeric tokens, depth 64, one million nodes, and 64 MiB of accounted work.
Reader limits are cumulative across the stream, including comments and discards;
configured depth is capped at 128. Exceeding a resource limit returns
`edn/capacity`.

Functions in `edb_core::edn_value` convert between data domains:

| Functions | Conversion |
| --- | --- |
| `edn_to_value`, `value_to_edn` | EDN and stored `Value`. |
| `edn_to_query_value`, `query_value_to_edn` | EDN and general `QueryValue`. |
| `query_result_to_edn` | Query results to EDN, including relations as sets of tuples. |
| `return_maps_to_edn` | Return maps to EDN with their key types and result shape. |

Each returns a `Result` and applies conversion limits. General maps, sets,
characters, nil, and arbitrary tags do not become stored scalar values merely
because the reader accepts them. Query conversion supports these broader data
shapes; it rejects map keys that collapse under native query equality.

For transaction data use `edb_core::edn_transaction::read_edn_transaction` or
`TransactionRequest::from_edn`; the outer form must be a list or vector.
For queries use `edb_core::edn_query::parse_query_edn` or `query_from_edn`.
These prepare data for their respective APIs; ordinary EDN reading does not
execute a transaction or query.
