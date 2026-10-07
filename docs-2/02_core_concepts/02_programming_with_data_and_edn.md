# Programming with Data and EDN

EDB is designed to be directly programmable using data. That means your application's code can use ordinary data structures to communicate with EDB. [Extensible Data Notation](https://github.com/edn-format/edn) (EDN) is a way of representing these data structures as text. Example transactions, queries, query results, and sample data throughout this guide are presented in EDN. You can also construct the necessary data structures programmatically using EDB's Rust APIs.

This section explains how to read EDN notation and how to use data structures to program with EDB. Even if you don't use EDN in your application's code, you'll want to be able to read EDN so that you can understand the examples in this guide. For an executable introduction, see the [EDN workflow](../01_tutorials/00_edn_workflow.md).

## EDB Data Types

The sections below summarize the data types used by EDB, and explain the EDN notation used to represent each type.

Following EDN convention, the examples in this guide use whitespace instead of commas to separate elements in collections. You may use commas to separate elements in collections if you wish; however, they will be ignored by the reader. EDN treats commas as whitespace unless they appear inside a string. A semicolon starts a comment that continues to the end of the line.

For more detailed information about the notation, refer to the [EDN specification](https://github.com/edn-format/edn).

EDB distinguishes parsed EDN data (`EdnValue`), stored attribute values (`Value`), and general query data (`QueryValue`). Reading valid EDN does not imply that every element can be stored as an attribute value. The [schema reference](../03_schema/02_schema_reference.md) describes the stored types; [general query data](../05_query_and_pull/01_application_data.md) describes the broader query domain.

### Scalar Data Types for Numbers

EDB uses the following data types to store numbers:

- **long** — A fixed, signed integer in two's complement binary representation. Corresponds to a Rust `i64`.
- **bigint** — An arbitrary precision signed integer. Represented by `num_bigint::BigInt`.
- **double** — A double-precision 64-bit IEEE 754 floating point number. Corresponds to a Rust `f64`.
- **float** — A single-precision 32-bit IEEE 754 floating point number. Corresponds to a Rust `f32`.
- **bigdec** — An exact decimal number with an arbitrary precision integer coefficient and a signed 64-bit scale. Represented by `bigdecimal::BigDecimal`.

| Data Type | Precision | EDN Notation | Example |
| --- | --- | --- | --- |
| long | 64-bit signed integer | digits 0–9, optional plus sign (+) or minus sign (-) prefix | `42`, `+42`, `-42`, `0` |
| bigint | arbitrary precision signed integer | same as long with a mandatory `N` suffix | `42N`, `+42N`, `-42N`, `0N` |
| double | 64-bit, double | digits with a decimal point or exponent, optional sign | `3.14`, `-3.14`, `1e3` |
| float | 32-bit, single | `#edb/float` followed by a string or a double literal | `#edb/float "3.13"`, `#edb/float -3.14` |
| bigdec | arbitrary coefficient, exact decimal | decimal or integer notation with a mandatory `M` suffix | `3.14M`, `-3.14M`, `42M` |

Ordinary decimal-point numbers are read as doubles. Use the explicit float tag when a stored float is required. Float conversion permits ordinary binary32 rounding, but rejects overflow and nonzero underflow. Unsuffixed integers outside the signed 64-bit range are rejected; use the `N` suffix for larger integers. Big decimals retain their scale across printing, including compact exponent forms such as `1e100M`.

Standard EDN numeric literals must be finite. EDB's `#edb/float-bits` and `#edb/double-bits` tags represent exact IEEE bit patterns, including infinities and NaN payloads. These and the other native tags are described in the [EDN workflow](../01_tutorials/00_edn_workflow.md#edn-types-versus-stored-values).

### Other Scalar Data Types

EDB's EDN reader uses the following non-numeric scalar data types (syntax indicated in the table below):

- **string** — A string.
- **character** — A single Unicode scalar value, represented by a Rust `char`.
- **boolean** — A boolean.
- **instant** — An instant in time (not a date).
- **uuid** — A universally unique identifier. Stored as its 128 bits in a Rust `u128`.
- **symbol** — Symbols are used to represent identifiers, similar to programming language keywords and variable names. Symbols may include an optional namespace in the form `namespace/name`.
- **keyword** — Keywords are identifiers that typically designate themselves, similar to programming language enumerations. Keywords follow the rules of symbols, except they must begin with a colon (:). May include an optional namespace in the form `:namespace/name`.
- **nil** — The absence of a value.

Having dedicated non-string types for symbols and keywords makes EDN especially suitable for domain-specific languages while retaining its all-data nature, and EDB uses this for query and transaction syntax.

| Data Type | EDN Notation | Example |
| --- | --- | --- |
| string | enclosed in double quotes | `"this is a string"` |
| character | preceded by a backslash | `\f`, `\tab`, `\return`, `\u001B` |
| boolean | `true`, `false` | `true`, `false` |
| instant | `#inst` followed by a string in RFC 3339 format | `#inst "1985-04-12T23:20:50.52Z"` |
| UUID | `#uuid` followed by a string in canonical UUID format | `#uuid "f81d4fae-7dec-11d0-a765-00a0c91e6bf6"` |
| symbol | begins with a non-numeric character other than a colon or hash sign; see the EDN specification for symbol characters | `?c`, `my-query-rules` |
| keyword | begins with a colon; see the EDN specification for keyword characters | `:find`, `:where`, `:db/id`, `:db/doc`, `:namespace/foo` |
| nil | `nil` | `nil` |

Characters and nil are valid EDN and general query values, but are not stored scalar attribute types. Stored tuples can contain permitted nil slots. Parsed `#inst` values retain their RFC 3339 precision; conversion to a stored instant uses milliseconds since the Unix epoch and rejects excess precision or leap seconds instead of rounding. `#edb/instant-millis` represents signed milliseconds directly, including dates outside the four-digit year range.

Additional stored types use namespaced tags: `#edb/ref` for entity references, `#edb/bytes` for hexadecimal bytes, `#edb/uri` for URI strings, and `#edb/function` for a native program's 32-byte content hash. A tag is data; reading it does not invoke a function or load code.

### Collection Data Types

EDB's EDN reader uses the following data types for multi-valued collections:

- **list** — A list is a sequence of values. Lists are represented by zero or more elements enclosed in parentheses (). Lists may contain heterogeneous elements and scalar or nested values.
- **vector** — A vector is a sequence of values. Vectors are represented by zero or more elements enclosed in square brackets []. Vectors may contain heterogeneous elements, and simple or nested values.
- **map** — A map is a collection of associations between keys and values. Maps are represented by zero or more key/value pairs, entirely enclosed in curly braces {}. Keys and values can be elements of any EDN type. Maps may contain heterogeneous elements, and simple or nested values.
- **set** — A set is a collection of distinct values, represented by zero or more elements enclosed in `#{}`.

| EDN Data Type | EDN Notation | Example in EDN |
| --- | --- | --- |
| list | enclosed in parentheses () | `(1 2 3)`, `(a b 42)` |
| vector | enclosed in square brackets [] | `[1 2 3]`, `[a b 42]` |
| map | enclosed in curly braces {} | `{:a 100 :b 90}`, `{:a 1, "foo" :bar, [1 2 3] four}` |
| set | enclosed in `#{}` | `#{1 2 3}`, `#{:red :green :blue}` |

The reader rejects duplicate map keys and duplicate set elements. Lists and vectors compare as sequences; maps and sets compare independently of element order. EDN numeric precision domains remain distinct: `42` and `42N` are different EDN keys. Native query equality has different numeric rules, and converting a map whose keys collapse under that equality fails explicitly.

Lists and vectors are interchangeable where an EDB grammar calls for a sequence, such as the outer transaction collection. Their role still depends on context: query syntax uses list forms for function expressions and vector forms for data patterns. A transaction map describes assertions about an entity; it is not a map stored as a single attribute value. Likewise, collections of cardinality-many values and stored tuples have different meanings. See [transaction data](../04_transactions/04_transaction_data.md) for those distinctions.

## Query EDN Example

Below is an example of a query shown in EDN. This query finds the IDs of all entities with a `:community/name` attribute whose value is "belltown".

```edn
[:find ?c :where [?c :community/name "belltown"]]
```

In the example above:

- `:find` and `:where` are keywords denoting programmatic clauses in the query.
- `?c` is a symbol denoting a logic variable.
- `:community/name` is a namespaced keyword, used as a constant in the query, and denoting the [ident](../03_schema/01_identity_and_values.md) of a particular schema attribute.
- `"belltown"` is a string, used as a constant in the query, and denoting the desired value of the `:community/name` attribute.
- The entire query is a vector containing a nested vector.

The attribute must be installed in the database supplied to the query. See the [query reference](../05_query_and_pull/05_query_reference.md) for more information about query syntax.

## Transaction EDN Example

Below is an example of a transaction written in EDN notation. Assuming the attributes are installed, entity `99` identifies the contributor, and no identity-unique value resolves to an existing entity, this transaction creates a new entity with two attributes: `:person/name` and `:person/email`.

```edn
[[:db/txInstant 99]
 {:db/source 99 :person/name "Anna" :person/email "anna@example.com"}]
```

In the transaction above:

- `:person/name` and `:person/email` are namespaced keywords denoting the idents of attributes the new entity will possess.
- `"Anna"` is a string denoting the value of the `:person/name` attribute.
- `"anna@example.com"` is a string denoting the value of the `:person/email` attribute.
- `:db/source` supplies the source entity ID for each map's assertions; it is transaction syntax, not a stored attribute.
- `[:db/txInstant 99]` asserts the transaction's selected instant with source `99`. It does not supply a default source for the map's assertions.
- The entire transaction is a vector containing a `:db/txInstant` form and an entity map.

Check [transaction structure](../04_transactions/04_transaction_data.md) for more information about how transactions are structured, including implicit temporary IDs and upserts.

## Programming with EDN in Rust

Read text into ordinary data with `read_edn`, or construct the same data directly:

```rust
use edb_core::Keyword;
use edb_core::edn::{EdnValue, read_edn, write_edn};

let text = r#"[{:person/name "Anna" :person/email "anna@example.com"}]"#;
let parsed = read_edn(text)?;
let constructed = EdnValue::Vector(vec![EdnValue::Map(vec![
    (EdnValue::Keyword(Keyword::new("person", "name")),
     EdnValue::String("Anna".into())),
    (EdnValue::Keyword(Keyword::new("person", "email")),
     EdnValue::String("anna@example.com".into())),
])]);
assert_eq!(parsed, constructed);
assert_eq!(read_edn(&write_edn(&constructed)?)?, constructed);
```

Reading is data-only: symbols and lists never execute. `read_edn` reads exactly one value and rejects trailing forms; `EdnReader` iterates a stream of values. Unknown tags are preserved by default. `EdnReadOptions` can reject them or supply explicit native tag handlers. Clojure reader macros, such as quoting and auto-resolved keywords, are not EDN syntax.

Use `parse_query_edn` or `query_from_edn` to prepare query data, `TransactionRequest::from_edn` to construct a submission, and `DatabaseValue::with_edn` to apply a speculative transaction to an immutable database value. The typed `Query`, `TxForm`, and `Value` APIs provide programmatic construction without a text round trip. The [native library example](../01_tutorials/00_edn_workflow.md#native-library) shows parsing, binding, execution, and result printing together.

Reading and printing enforce resource limits. Default limits include 16 MiB of input/output, depth 64, one million nodes, numeric tokens up to 64 KiB, and cumulative format work up to 64 MiB. Conversion to stored or query values and transaction/query execution have their own admission limits. See [limits and errors](../01_tutorials/00_edn_workflow.md#limits-errors-and-evidence) for details.

## EDN and Stored Representations

EDN represents ordinary data as text. Stored values use a native typed binary encoding shared by durable transactions and index values. The EDN reader's entire domain does not become a stored attribute-value domain: reading a character, general map, set, unknown tag, or empty vector does not make it a stored value. The schema's `:db/valueType` vocabulary governs stored attributes.

Exact numeric values and named data retain their type information in native encoding:

| Value information | Native durable representation and boundary |
| --- | --- |
| BigDecimal | Signed coefficient plus signed scale; no conversion through double |
| BigInteger | Minimal signed big-endian bytes; redundant sign bytes rejected on decode |
| Keyword / symbol | Distinct tag, optional namespace and name; not flattened to a string |
| Instant / UUID | Signed millisecond integer / 128 original bits |
| URI | Original spelling; syntax validated separately from component equality |
| Float / double | Values encoded at their width; signed zero and NaNs canonicalized |
| Function | Immutable native program's content hash |

Floating-point exactness has an important boundary. The EDN bit tags can preserve NaN payload bits on the EDN conversion path, but durable and request bytes normalize zero signs and NaN payloads. They do not preserve every accepted floating-point representation bit for bit. See [value identity](../03_schema/01_identity_and_values.md#nan-replace-ordinary-values-never-use-as-a-unique-identity) for logical equality and uniqueness rules, and [URI identity](../03_schema/01_identity_and_values.md#uri-identity-and-exact-input) for the distinction between original spelling and component equality.

Typed decoding, integrity checks, and physical compression are separate layers. Canonical value decoding consumes exactly one value and requires byte-identical re-encoding. Durable transaction envelopes separately check kind, current version, reserved byte, length, and SHA-256 checksum.

Opaque storage objects are identified by the SHA-256 of their canonical bytes, independent of physical compression; identical payloads share an object identity. Storage keeps raw content unless gzip plus its envelope is smaller. Decoding bounds declared output, checks gzip completion, rejects trailing data or another member, and checks the canonical SHA-256. Index and log consumers receive canonical bytes. See [authoritative object compression](../08_operations/00_deployment.md#authoritative-object-compression) for storage limits and backup behavior.

A checksum checks agreement with an expected content identity. An unkeyed checksum does not authenticate an arbitrary sender, and successful decoding does not establish a durable transaction acknowledgement. Publication ordering, writer authority, and success acknowledgement are part of the [transaction protocol](../04_transactions/08_acid.md).
