# Transaction source and timestamp

Each transaction has one explicitly supplied source. Every assertion and
retraction it produces carries that source, including its transaction instant,
annotations, nested entity facts, function results, and generated changes.

Source identifies the contributor of the database statements. Domain attributes
describe distinct roles such as a work's author, performer, or publisher.
See [NORTHSTAR](NORTHSTAR.md#music-and-books-authorship-and-source).

## Transaction input

The application supplies source once alongside transaction data. Rust requests
carry `source: EntityRef`; transaction forms retain their ordinary arguments.

```rust
let request = TransactionRequest::from_edn(
    "catalog-import-1",
    r#"[{:db/id 42 :painting/year 1916}]"#,
    EntityRef::Id(99),
)?;
```

The equivalent CLI submission supplies an EDN entity reference with `--source`:

```sh
edb transact --database catalog --endpoint /tmp/catalog/writer.sock \
  --request-key catalog-import-1 --source 99 --file painting.edn
```

Source follows ordinary entity-reference resolution. It may be an existing id,
ident, lookup reference, or a tempid established in entity position in the same
transaction. It resolves to the source entity id stored in each resulting
datom. Transaction functions produce ordinary forms within the enclosing
transaction; their output receives that transaction's source.

The supplied source is part of request identity, alongside the transaction
data, basis condition, and explicit time option. Reusing a request key requires
the same complete request. Exact retries return the accepted result before
fresh entity resolution or clock selection.

## Automatic timestamp

The writer selects one transaction instant and EDB automatically asserts it on
the current transaction entity. The assertion receives the transaction's source.
For a transaction from source `99`, the resulting datoms include:

```text
[42 :painting/year 1916             tx 99 true]
[tx :db/txInstant selected-instant  tx 99 true]
```

There is one timestamp assertion per transaction. Empty transaction data is
valid and still produces that assertion. Adding an annotation or rearranging
transaction forms does not change timestamp attribution.

Clock selection retains the existing rules:

- Durable transactions normally use the writer's clock, clamped to the previous
  transaction instant when necessary to keep time nondecreasing.
- An explicit instant must not precede the previous transaction instant or
  exceed the writer's clock. A value supplied through transaction data must
  agree with an explicit request-level time override.
- Speculative transactions use the fixed instant supplied to the `with` call.
  An explicit timestamp assertion must agree with that instant, which must not
  precede db-before.

The source records responsibility for the transaction. The clock supplies the
timestamp value. Speculation supplies that value through its existing time
argument and uses the source supplied for the speculative transaction.

## Explicit timestamp values

An import may supply a timestamp value through ordinary transaction data:

```edn
[{:db/id "edb.tx"
  :db/txInstant #inst "2026-10-01T12:00:00Z"}
 {:db/id 42
  :painting/year 1916}]
```

Submit this data with one source through the request or CLI. EDB validates the
explicit instant and records one timestamp assertion with that source.
Timestamp assertions apply only to the current transaction. Retraction and
compare-and-swap of `:db/txInstant` are prohibited.

## Stored facts and history

Datoms retain the form `[e a v t s added?]`. All datoms produced by a transaction
have its transaction id and source id; `added?` is a boolean. Existing facts
retain their original transaction and source when a later transaction changes
the record. Retractions record the withdrawing transaction's source.

Different transactions may supply the same E/A/V from different sources.
Those contributions remain distinct, while cardinality continues to count
distinct values in the shared current record.

Canonical genesis contains system-attributed vocabulary and schema facts
without a transaction timestamp. Internal bootstrap transactions use the
ordinary source entity `:db.source/system`. Application transactions supply
their own source and receive the automatic timestamp under the rules above.
