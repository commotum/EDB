# Attribution for the transaction instant

Decision: use the dedicated transaction form `[:db/txInstant source]` to request an assertion of the current transaction's selected instant, attributed to the supplied source entity. Its native representation is `TxOp::TxInstant { source: EntityRef }`.

This form supplies attribution for one assertion. It does not set a source default for other transaction data. Ordinary entity maps continue to abbreviate their attribute assertions, including maps describing the transaction entity.

## The problem

EDB's source model requires each assertion and retraction to carry an explicitly supplied source entity ID. The transaction's timestamp is also an assertion. Its entity and attribute are known, and the existing clock rules select its value; the missing input is its source.

Applications need to provide that attribution while retaining automatic clock selection, independent attribution for other facts, and ordinary transaction composition. Adding annotations or rearranging equivalent transaction data must not change timestamp attribution.

The earlier documentation gave a source-only transaction map a special role:

```edn
{:db/id "edb.tx" :db/source 99}
```

It attributed the generated timestamp only while the map had no attribute entries. Adding `:db/doc "Import"` changed its role and required another timestamp declaration. That dependence on unrelated attributes was the defect.

## Basis for the decision

The investigation used the describe, diagnose, delimit, direction and design stages from Rich Hickey's [Design in Practice](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/DesignInPractice.md), including a problem statement, competing approaches, concrete use cases and an executable model. The assessment focused on independent responsibilities and the ability to compose and change the resulting system, following [Simple Made Easy](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimpleMadeEasy.md) and [Simplicity Matters](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimplicityMatters.md). The choice of a dedicated form is an EDB design judgment; the talks do not prescribe that syntax.

Datomic's conventions establish useful boundaries: transaction data is unordered, entity maps expand into attribute additions, transaction annotations are ordinary facts, and a transaction's automatically selected instant can be replaced by an explicit instant. Datomic does not have EDB's source coordinate or the dedicated form chosen here. See its [transaction data reference](https://docs.datomic.com/transactions/transaction-data-reference.html#reified-transactions).

The EDB implementation already separates normalized transaction forms, clock selection and assessment in [the transaction pipeline](src/transaction/pipeline.rs). Its [transaction input types](src/transaction/forms.rs) distinguish unresolved input from stored values and include explicit directives. Those boundaries support a dedicated form without replacing existing submission or speculation methods.

## Alternatives examined

| Approach | What it provides | Consequence |
| --- | --- | --- |
| Source-only transaction map | Attribution without a new syntactic form | Adding an unrelated annotation changes whether the map supplies timestamp attribution. |
| Every transaction map supplies timestamp attribution | Stable behavior when a map gains an attribute | An annotation by another source also creates a timestamp contribution by that source. Equivalent primitive additions no longer have the same effect as the map. |
| Automatic system source | No additional application input | Requires an exception to the settled requirement for explicitly supplied attribution. |
| Literal timestamp assertion on every submission | Ordinary assertion syntax throughout | Transfers clock selection and time-ordering concerns to applications; suitable for imports, but does not preserve normal automatic clock selection. |
| Request field such as `tx_instant_source` | A named input for the generated fact | Requires another input channel across submission, speculation and CLI interfaces. A singleton field cannot compose several source contributions; transaction functions return forms rather than request options. |
| Deferred transaction-instant value in Add/map | Attribution stays in ordinary assertion syntax | Introduces a value legal only in particular operation, entity and attribute positions, with rules for rejecting it elsewhere. |
| Dedicated timestamp form | A composable request for the selected instant with an explicit source | Adds one specialized form and native operation, plus encoding and persisted-program support. |

The deferred-value alternative remains credible because EDB already resolves transaction-only values before storage. It would become more attractive if using the selected transaction time in arbitrary domain attributes were a requirement. For the present scope, the dedicated form directly represents its legal entity/attribute/value combination without introducing a broader value category and restricting most uses of it.

The dedicated form also carries the complete assertion request through the transaction-data collection already accepted by native, EDN, CLI and transaction-function interfaces. No request-level source default is needed.

## Accepted contract

```text
EDN:    [:db/txInstant source]
Native: TxOp::TxInstant { source: EntityRef }
VM:     Instruction::EmitTxInstant, consuming [source]
```

The form means: assert the selected transaction instant on the current transaction entity, with the supplied source. It does not read the clock when encountered or depend on its position among transaction forms.

The source follows the ordinary transaction entity-reference rules. It resolves to an explicitly supplied entity ID. A source tempid must also be established in entity position in the same transaction. Source resolution precedes deduplication, so different references resolving to the same entity identify the same source.

Clock selection retains existing behavior:

- Durable transactions use the writer's clock and existing nondecreasing-time rule unless an allowed explicit instant is supplied.
- Explicit timestamp assertions and an explicit request-level time override must agree on one value and satisfy the existing time bounds.
- Speculative transactions use the fixed instant supplied to the existing `with` family of methods. Explicit timestamp assertions must agree with that value.

After selecting the instant, each dedicated form contributes an ordinary sourced timestamp assertion. An explicit sourced `:db/txInstant` assertion is sufficient by itself; it requires no dedicated form. A request-level time override supplies a value but still requires attribution through transaction data.

Each submitted transaction must produce at least one sourced timestamp assertion, including transactions with no domain changes. A transaction-entity map containing only `:db/id` and `:db/source` produces no assertion and has no timestamp-declaration role. Sources on annotation maps apply to their own assertions.

One transaction has one distinct timestamp value. It can retain several explicitly supplied sources for that value:

| Submitted timestamp intent | Result |
| --- | --- |
| Dedicated form from S1 | Selected instant attributed to S1. |
| Dedicated forms from S1 and S2 | Same selected instant attributed separately to S1 and S2. |
| Repeated dedicated forms from S1 | One assertion after ordinary deduplication. |
| Explicit instant I from S1 | I attributed to S1. |
| Explicit instant I from S1 and a dedicated form from S2 | I attributed separately to S1 and S2. |
| Explicit instant I from S1 and explicit I from S2 | Both source contributions retained. |
| Distinct explicit instants I and J | Transaction rejected. |
| Transaction annotation from S1 | Only that annotation is attributed to S1; it supplies no timestamp attribution. |

Retraction and compare-and-swap of `:db/txInstant` remain prohibited. Timestamp assertions apply only to the current transaction. The final datom slot remains a boolean.

## Examples

Assume source entities `99`, `77` and `101`, painting entity `42`, and the attributes already exist:

```edn
[[:db/txInstant 99]
 {:db/id "edb.tx"
  :db/source 77
  :db/doc "Reviewed museum import"}
 {:db/id 42
  :db/source 101
  :painting/year 1916}]
```

Schematically, the assertions are:

```text
[tx :db/txInstant selected-instant        tx 99  true]
[tx :db/doc       "Reviewed museum import" tx 77 true]
[42 :painting/year 1916                  tx 101 true]
```

Removing the annotation removes only the second assertion. Reordering the forms leaves their meaning unchanged.

A transaction with no domain changes can contain just:

```edn
[[:db/txInstant 99]]
```

An import can instead assert its timestamp directly, subject to the destination's existing time bounds:

```edn
[{:db/id "edb.tx"
  :db/source 101
  :db/txInstant #inst "2026-10-01T12:00:00Z"}]
```

Adding `[:db/txInstant 99]` to that import contributes source `99` to the same selected historical instant; it does not replace the explicit assertion's source or select a second time.

## Implementation consequences

The source-aware implementation will need to:

1. Add the native operation, EDN lowering and persisted-program emission instruction. `EmitTxInstant` consumes one source entity reference and emits the dedicated operation; it does not inspect the clock.
2. Select one instant under the existing clock rules, then lower dedicated forms to ordinary sourced additions before source resolution and assessment. Primitive-only speculation must use the same lowering rule.
3. Replace the unconditional timestamp append with validation and materialization of explicitly attributed timestamp assertions.
4. Replace timestamp-datom count restrictions with agreement on one distinct instant. Existing checks occur in [clock selection](src/transaction/pipeline.rs), [assessment](src/transaction/assess/mod.rs), and [history/recovery validation](src/database.rs).
5. Include the new operation in canonical request encoding, wire formats, resource accounting and persisted-program encoding. Exact receipt lookup must remain ahead of fresh source resolution and clock selection, preserving the accepted timestamp and attribution on retry.
6. Preserve existing submission signatures, report shapes, source-reference resolution and time-boundary semantics. Convenience helpers that create transactions must emit an explicitly sourced timestamp form or assertion.

Canonical genesis is a separate bootstrap case: the current implementation's genesis contains vocabulary and schema facts without a transaction timestamp. A later schema-installation transaction remains subject to ordinary timestamp attribution. See [database construction](src/database.rs) and [the built-in vocabulary](src/model/vocabulary.rs).

## Evidence and limits

A small executable model reproduced the old empty-map failure and the incidental attribution caused by treating every transaction map as a timestamp declaration. The dedicated-form and deferred-value alternatives both preserved annotation independence, equivalent map expansion, multiple sources, duplicate elimination and explicit time overrides. Each preserved its assertion set across all 120 permutations of a five-form transaction.

These experiments checked the candidate semantics, not an implemented EDB feature. The structural comparison and existing API boundaries determined the choice between the two successful candidates. This report records the decision; the accompanying documentation describes the accepted behavior. Implementation and engine-level validation remain a separate code refactor.
