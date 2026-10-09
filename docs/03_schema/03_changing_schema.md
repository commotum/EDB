# Changing schema

Changing an existing schema attribute is similar to defining a new schema
attribute: you submit a transaction with the new facts, including the `:db/id`
and the new value(s) of the attribute(s), and supply one source for the
transaction. See [schema as database data](00_schema.md) for installation and the
[schema data reference](02_schema_reference.md) for
individual properties and value types.

Logical schema changes happen synchronously with the transaction. The returned
db-after contains the new schema. Physical indexing work can complete later;
adding an AVET index has a separate readiness condition described below.

A database value uses the schema associated with its captured basis. Applying
`as_of`, `since`, or `history` changes which facts are visible, not the working
schema of that value. Thus traveling back in time does not take the working
schema back in time. A database value captured before a schema change remains
immutable and retains its earlier schema; obtaining a newer value is different
from applying a time filter to it.

Attempting a schema change that violates an invariant causes the transaction to
abort. To enable such a change, first change your data so the invariant becomes
true. For example, before marking an attribute unique, find and retract duplicate
current values. Cardinality and uniqueness transition checks examine the proposed
resulting facts, so compatible cleanup retractions can also be included in the
schema transaction. Neither a rejected change nor its accompanying cleanup is
partially committed.

Transaction forms resolve against db-before. The old schema controls input
interpretation, tempid upsert, cardinality-one replacement, component retraction,
and composite derivation during the transaction that changes it. The resulting
schema and facts must also pass successor validation. Commit a schema change
before submitting data that relies on its new transaction behavior.

The examples below assume the named attributes already exist with the
appropriate value types and source entity `99` identifies the contributor.
Submit each vector as ordinary EDN transaction data with source `99` supplied
once through the transaction request or CLI `--source 99`. Every explicit and
generated change carries that source, including the automatic transaction
timestamp.

## Changing `:db/ident`

This example changes the `:db/ident` of `:person/name` to `:person/full-name`:

```edn
[{:db/id     :person/name
  :db/ident  :person/full-name}]
```

Idents provide names for things, specifically schema attributes and enumerated
values. Both types of names can be changed by changing their `:db/ident` fact.
Submit the entity's existing `:db/id` and its new `:db/ident`. The change is
immediately visible in the database returned by the transaction.

Both the new ident and the old ident refer to the entity. `DatabaseValue::entid`
resolves both names to the same entity ID. `DatabaseValue::ident` returns the new
name; the current `:db/ident` fact is also the new name. Attribute renaming does
not rewrite application datoms: they continue to use the same attribute ID.

Avoid repurposing an old ident. If you need to reuse the name for a different
entity, assert that name on the other entity. The later assertion takes over
the old alias, which then ceases to resolve to its previous entity. Existing
clients that still use the old name will follow its new meaning. An ident rename
is therefore safer than a name reuse when migrating application code.

## Changing `:db/cardinality`

The example below changes the cardinality of `:person/favorite-food` to many and
the cardinality of `:person/email` to one:

```edn
[{:db/id          :person/favorite-food
  :db/cardinality :db.cardinality/many}
 {:db/id          :person/email
  :db/cardinality :db.cardinality/one}]
```

When changing from a multi-valued attribute to a single-valued attribute, there
must be at most a single distinct value for every entity in the resulting current
assertions. Otherwise the change fails with `schema/cardinality-change-conflict`.
EDB does not choose which current value to retain. Retract unwanted values before
the change or include their retractions in the same transaction.

An attribute that is unique must remain cardinality-one. An active composite
tuple also requires each constituent to remain a cardinality-one tuple scalar.
Remove the conflicting uniqueness constraint, or discontinue the dependent
composite, before changing such an attribute to many.

After changing cardinality, entity navigation returns the shape specified by the
new schema: a scalar or reference for one, and a collection for many. This also
applies to time-filtered values derived from the new basis. An entity viewed
through `as_of` or `since` can have multiple historical values but return only one
through cardinality-one entity navigation. Use datom or query access when all
historical facts matter; an entity projection does not preserve every historical
event. See [immutable reads](../02_core_concepts/00_database_values.md) and
[Pull and entities](../05_query_and_pull/02_pull_and_entities.md).

## Changing `:db/isComponent`

The example below changes `:order/line-items` to be a component, and
`:customer/orders` to not be a component:

```edn
[{:db/id          :order/line-items
  :db/isComponent true}
 {:db/id          :customer/orders
  :db/isComponent false}]
```

Changing `:db/isComponent` causes subsequent transactions and reads of the new
database value to use the new component semantics. For example, when it is true,
`[:db/retractEntity parent-id]` recursively retracts the referenced component
entities, attributing each retraction to source `99`. `:db.fn/retractEntity` is
also accepted. Component navigation and Pull use the flag from their supplied
database value.

Only a reference-valued attribute can be a component. Omitting the flag or
setting it to false disables component behavior. Changing the flag alone does
not retract existing entities or establish exclusive ownership of them. If the
same transaction changes the flag and retracts a parent, retraction expansion
uses the old flag from db-before; use a later transaction to apply the new
behavior.

## Changing `:db/noHistory`

The example below changes `:person/encrypted-password` to permit omission of old
history, and changes `:person/address` to retain history going forward:

```edn
[{:db/id :person/encrypted-password
  :db/noHistory true}
 {:db/id :person/address
  :db/noHistory false}]
```

Changing an attribute's `:db/noHistory` to false, or retracting its true value,
causes future indexing to retain history for that attribute. Setting it to true
allows indexing to omit eligible assertion/retraction pairs. The flag does not
synchronously erase current historical values, and it does not guarantee when
an old fact disappears from retained indexes. Later index consolidation can
also omit eligible older pairs encountered in rewritten segments.

Changing the flag back to false does not restore history previously omitted.
Already captured values do not change in place. Transaction logs, receipts,
backups, and copied data are not erased by `:db/noHistory`; use the separate
[excision workflow](../08_operations/00_deployment.md#excision) when appropriate.

## Adding an AVET index to an attribute

If `:db/index` is true or `:db/unique` is set, EDB requests maintenance of the
attribute's values in the [AVET index](../06_indexes/00_indexes_and_log.md).
When you alter an existing attribute to be maintained in AVET, its backfill may
not be available immediately.

The example below requests an AVET index for `:person/external-id`:

```edn
[{:db/id :person/external-id
  :db/index true}]
```

After the transaction commits, use `Connection::sync_schema(target_t, timeout)`
or `Peer::sync_schema(target_t, timeout)` with that transaction's logical T to
wait for required AVET additions through the target. Use the database value
returned by the wait. `DatabaseValue::has_avet` accepts an `AttributeName::Ident`
or `AttributeName::Id` and reports physical readiness for that captured value;
the schema flag alone does not prove readiness. Unknown attributes produce an
error rather than a false readiness result.

A captured pending value does not become ready in place when indexing finishes.
Acquire a newer value to observe publication. A timeout reports
`peer/sync-schema-timeout`; it does not undo the schema transaction. The wait
requires the writer's background indexing to make progress. An attached
connection can explicitly request indexing through `Connection::request_index`;
see [maintenance controls](../08_operations/04_maintenance_controls.md).

## Removing an AVET index from an attribute

If `:db/index` is false or unset and `:db/unique` is unset, EDB no longer requests
AVET membership. The example below retracts the unique identity constraint and
the explicit index flag for `:person/external-id`:

```edn
[[:db/retract :person/external-id :db/unique :db.unique/identity]
 [:db/retract :person/external-id :db/index true]]
```

Retract the actual unique mode if the attribute instead uses `:db.unique/value`.
Removing only the index flag leaves AVET required by uniqueness. Removing only
uniqueness leaves AVET required by an explicit true index flag.

EDB synthesizes attribute alteration hooks for changed schema properties;
ordinary EDN changes do not need an additional
`[:db/add :db.part/db :db.alter/attribute :person/external-id]` form. Explicit
install or alter hooks, when supplied, must be assertions on `:db.part/db` and
must match whether the attribute was installed in db-before.

Logical removal and physical index publication are distinct. `sync_schema`
waits for additions, not removal completion. `Connection::sync_index` or
`Peer::sync_index` waits for indexing through the target, including applicable
AVET additions and removals. Previously captured values retain their own index
view.

## Changing `:db/unique`

This example changes the attribute `:person/external-id` to a unique identity:

```edn
[{:db/id :person/external-id
  :db/unique :db.unique/identity}]
```

To add a uniqueness constraint, all of the following must be true:

- The attribute must have cardinality `:db.cardinality/one`.
- Values must be unique across entities in the resulting current assertions.
- If the attribute has historical values, its AVET index must already be
  physically ready in db-before. Setting `:db/index true` in this same
  transaction is insufficient.
- The value type cannot be bytes, and values cannot be NaN.

For an existing unindexed attribute, transact `:db/index true`, wait for schema
readiness at that transaction's T, remove duplicate current values, and then
add uniqueness. An attribute with no historical values has nothing to backfill
and can acquire uniqueness without the separate index preparation. Simply
retracting all current facts does not remove the retained historical values
that require this preparation.

Adding uniqueness does not change history; historical databases may contain
duplicate values. Code that expects a unique value can find multiple owners
when querying historical facts.

`:db.unique/identity` enables tempid upsert. `:db.unique/value` enforces uniqueness
without merging a new tempid into an existing owner. Changing between the modes
uses an ordinary assertion of the new mode; identity resolution for that change
transaction still uses db-before. See [value and identity rules](01_identity_and_values.md)
for composite identities, URI equality, and NaN.

This example retracts the unique identity constraint going forward:

```edn
[[:db/retract :person/external-id :db/unique :db.unique/identity]]
```

The successor schema no longer enforces that constraint. Input resolution in the
removal transaction still uses the old unique mode, so submit later data under
the new schema when you need to create separate entities with equal values.

## Immutable properties and discontinuing tuples

You cannot alter an installed attribute's `:db/valueType`, `:db/fulltext`,
`:db/tupleAttrs`, `:db/tupleTypes`, or `:db/tupleType`. Create a new attribute and
migrate the data when one of those definitions must change. Installed attributes
cannot be removed from schema, and native system attributes cannot be altered.
An unused attribute can remain installed while applications stop writing it.

You can permanently discontinue an installed composite tuple:

```edn
[[:db/add :semester/year+season :db.tuple/discontinued true]]
```

Subsequent transactions stop deriving its values when constituents change.
Existing tuple datoms remain. Discontinuation releases the active constituent
dependencies, so former constituents can subsequently change cardinality.
The tuple definition itself remains immutable. Discontinuation applies only to
composite tuples, cannot be set during installation, and cannot be reversed by
setting false or retracting true. Create a new composite attribute if automatic
derivation is needed again.

Installing a new composite does not populate every existing entity. After
installation, reassert a constituent value on each target entity to derive its
tuple, even when a same-source assertion is otherwise redundant. Supply the
source once for each transaction; derived assertions and retractions carry that
source. Contributions from different sources in separate transactions remain
distinct. Process large populations in bounded transactions and handle any
derived uniqueness conflicts.
See [schema data reference](02_schema_reference.md) for composite declarations
and slot rules.

## Diagnosing a rejected change

The transaction error identifies the violated invariant. Common errors include:

| Error code | Meaning and remedy |
| --- | --- |
| `schema/cardinality-change-conflict` | More than one value remains for an entity; retract extras before changing to one. |
| `schema/unique-change-conflict` | Different entities retain the same value; resolve the duplicates. |
| `schema/unique-requires-avet` | Historical values exist without ready AVET; enable indexing and wait before adding uniqueness. |
| `schema/unique-must-be-cardinality-one` | A unique attribute is declared many; retain one or remove uniqueness. |
| `schema/component-must-be-ref` | A non-reference attribute is marked as a component. |
| `schema/value-type-immutable` | An installed value type changed; use a new attribute. |
| `schema/fulltext-immutable` | An installed fulltext flag changed; use a new attribute. |
| `schema/tuple-definition-immutable` | An installed tuple definition changed; use a new tuple attribute. |
| `schema/tuple-discontinuation-irreversible` | A discontinued composite was resumed; install a new one. |

Fix the data or schema proposal and submit a new transaction request. Repeating
an identical invalid proposal does not make it valid. The
[transaction guide](../04_transactions/00_transactions.md) explains request
identity and exact retries, and the [diagnostics guide](../04_transactions/03_diagnostics.md)
describes transaction errors and inspection.
