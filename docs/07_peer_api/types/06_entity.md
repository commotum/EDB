# Entity

`edb_core::Entity` implements the [Entity API](../../02_core_concepts/04_entities.md)
for associative navigation by attribute keys. An entity is lazy: the values of
its attributes are not obtained from the database until `get`, `get_named`,
`get_direction`, or `touch` is called, after which they are cached in the entity.

Entities have reference-like equality semantics. Two entities are equal if they
have the same entity ID and database origin. Equality and hashing exclude the
cache, attribute values, and time basis. Entities from different time views of
the same database can therefore compare equal even when their attributes differ.
Durable origin is the database lineage; independent in-memory databases have
distinct origins. See [entity identity and state equality](../../02_core_concepts/00_database_values.md#entity-identity-is-not-state-equality).

## Construction

Obtain an entity from a captured `DatabaseValue` or an in-memory `Database`:

```rust,ignore
fn entity(
    &self,
    identifier: impl Into<EntityIdentifier>,
) -> Result<Option<Entity>, SemanticError>;
```

The identifier can be a numeric `u64` ID, a `Keyword` ident, or an
`EntityIdentifier::Lookup { attribute, value }` lookup reference. An unresolved
ident or lookup reference returns `None`. A valid numeric ID returns an entity
even when no facts are available for that ID; this is not an existence test.
See [entity identifiers](../../05_query_and_pull/06_pull_reference.md#entity-identifiers-and-missing-entities).

`Entity::from_database_value(database: DatabaseValue, id: u64)` returns
`Result<Entity, SemanticError>`. This lower-level constructor checks that the
database is a point-in-time value and wraps the supplied ID directly; it does
not resolve or validate the ID through `DatabaseValue::entity`.

Entities retain their captured database value. Captured current values,
`as_of(t)`, `since(t)`, filtered values, and speculative transaction results can
back entities. A since view exposes only its visible facts. A `history()` value
cannot back an entity and returns `database/history-not-point-in-time`.

## Method summary

All methods below borrow `&self`.

| Method | Return type | Description |
| --- | --- | --- |
| `get(attribute: u32)` | `Result<Option<EntityValue>, SemanticError>` | Gets the value of an attribute by numeric ID. |
| `get_named(attribute: &AttributeName)` | `Result<Option<EntityValue>, SemanticError>` | Gets the value of a forward attribute by ID or keyword. |
| `get_direction(direction: &PullDirection)` | `Result<Option<EntityValue>, SemanticError>` | Gets a forward or reverse attribute value. |
| `touch()` | `Result<Self, SemanticError>` | Touches all the attributes of the entity, including component entities recursively. |
| `keys()` | `Result<BTreeSet<Keyword>, SemanticError>` | Returns available forward attribute keys, including `:db/id`. |
| `database()` | `DatabaseValue` | Returns the database value that is the basis for this entity. |
| `id()` | `u64` | Returns the entity ID. |
| `identity()` | `EntityIdentity` | Returns a lightweight identity token without reading attributes. |
| `reverse(attribute: u32)` | `Result<Vec<Entity>, SemanticError>` | Returns entities pointing to this entity through the reference attribute. |
| `pull(pattern: &PullPattern)` | `Result<QueryValue, SemanticError>` | Selects data using a Pull pattern and this entity's captured database. |

`Entity` implements `Clone`, `Debug`, `PartialEq`, `Eq`, and `Hash`. Attribute
access uses the methods above, with explicit `Option` and `Result` return values.

## Method details

### `get`, `get_named`, and `get_direction`

Gets the value of the attribute named by the key. Cardinality-many attributes
always return a collection when present, even when only one value is present.
The result is `None` if the attribute has no values; a missing cardinality-many
attribute does not return an empty collection.

```rust,ignore
pub enum EntityValue {
    Scalar(Value),
    Entity(Entity),
    Collection(Vec<EntityValue>),
}
```

Use `get` for a numeric attribute ID. Use `get_named` with
`AttributeName::Ident(Keyword::new("user", "firstName"))` for `:user/firstName`,
or `AttributeName::Id(id)` for a numeric ID. Unknown keyword attributes behave
like missing keys; unknown numeric attribute IDs return an error.

The special forward key `:db/id` returns
`Some(EntityValue::Scalar(Value::Ref(entity.id())))`. Other scalar attributes
return `EntityValue::Scalar`. Reference attributes return lazy `Entity` values
with the same captured database, except that forward references to entities with
a `:db/ident` return that ident as `EntityValue::Scalar(Value::Keyword(...))`.
Pass the keyword to `entity.database().entity(keyword)` to obtain the identified
entity. Cardinality-many collections contain distinct values.

`get_direction` accepts:

- `PullDirection::Forward(attribute)` for explicit forward access.
- `PullDirection::Reverse(attribute)` for explicit reverse reference access.
- `PullDirection::SchemaResolved(keyword)` for schema-resolved spelling, such
  as `Keyword::new("person", "_likes")`. An installed attribute with that exact
  name takes precedence; otherwise the underscore requests reverse navigation
  through `:person/likes`.

`get_named` always requests forward access and does not interpret underscore
shorthand. Reverse navigation requires a reference attribute; reversing a
non-reference attribute returns `pull/reverse-non-ref`. Reverse references always
produce entities, including references from identified entities. A reverse
noncomponent attribute returns a collection regardless of forward cardinality;
a reverse component attribute returns a single parent entity. Missing incoming
references return `None`.

The result of each attribute direction, including absence, is cached in the
entity. A clone shares that cache; constructing another entity for the same ID
creates a separate cache. Numeric and keyword selectors are separate cache
keys. A cache miss on a storage-backed database may load immutable index nodes
from storage. See [laziness and caching](../../02_core_concepts/04_entities.md#laziness-and-caching).

### `touch`

Touches all the ordinary attributes of the entity, including any component
entities recursively. It returns a clone sharing the populated cache.
Noncomponent references remain lazy. Forward identified references remain
keywords, so touch does not descend through them. Touch follows forward
component edges, stops when a component path revisits an entity, and does not
traverse reverse edges.

For example, touching an order accesses its attributes and those of its component
line items. It does not access the attributes of products referenced by those
line items through noncomponent references. Use [Pull](../../05_query_and_pull/06_pull_reference.md)
when you want to select a particular set of attributes declaratively.

### `keys`

Returns the available forward keys as a `BTreeSet<Keyword>`, including `:db/id`.
It enumerates the visible attribute names without realizing or caching their
values. It does not enumerate reverse attributes. An entity with no visible
facts has only the `:db/id` key.

### `database`

Returns a clone of the database value that is the basis for this entity,
preserving its time boundaries and filters. This method does not refresh a
connection. All entity navigation stays within this captured value.
See [entities and time](../../02_core_concepts/04_entities.md#entities-and-time).

### `id` and `identity`

`id()` returns the numeric entity ID. `identity()` returns an `EntityIdentity`
using the same database-origin and entity-ID equality as `Entity`, without
reading attributes. `EntityIdentity::entity_id()` returns the ID and
`EntityIdentity::lineage_id()` returns `Option<&str>`: the durable lineage, or
`None` for a standalone in-memory origin. Include snapshot information separately
when a key must distinguish time views; compare selected Pull or query results
when comparing attribute state.

### `reverse`

Convenience access for incoming references by numeric attribute ID. It returns
a `Vec<Entity>` for both component and noncomponent attributes, with an empty
vector when there are no incoming references. It uses the same cache and
reference validation as `get_direction`.

### `pull`

Equivalent to `entity.database().pull(pattern, entity.id())`. It returns the
selected data as a `QueryValue`, using the [Pull pattern](../../05_query_and_pull/06_pull_reference.md)
and the entity's captured value. The Pull operation reads through the database
API; it does not populate the entity's attribute cache.
