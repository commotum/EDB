use crate::model::identity::{validate_frontier, validate_supported_eid};
use crate::{
    Attribute, AttributeRef, CallableRef, Cardinality, Datom, EntityMap, EntityRef, ErrorCategory,
    IndexOrder, Instruction, Keyword, MAX_EIDX, MapValue, PROGRAM_ABI_VERSION, Program,
    ProgramCall, ProgramHash, ProgramKind, ProgramOutput, QueryTemplate, RuntimeValue,
    SemanticError, Symbol, TupleSpec, TxForm, TxOp, TxValue, Unique, Value, ValueType, eid_to_eidx,
    t_to_tx, tx_to_t,
};
use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

mod program_query;
mod submission;
pub(crate) use program_query::query_cache_key;
pub(crate) use program_query::validate_native_query;
pub(crate) use submission::{
    ExactEndpoint, WireOutcome, WireReport, decode_submission, decode_submission_outcome,
    encode_submission, encode_submission_outcome,
};

pub type Digest = [u8; 32];

const MAGIC: &[u8; 4] = b"EDBC";
// Genesis and schema are ordinary immutable information. Log serialization's
// stored-first ordering is distinct from index traversal's T/op-first ordering;
// index and database-root versions reject incompatible comparator generations.
// There is no old-format reader or administrative upgrade converter.
const FORMAT_VERSION: u16 = 4;
const HEADER_LEN: usize = 16;
const CHECKSUM_LEN: usize = 32;
const MAX_BLOB_LEN: usize = 64 * 1024 * 1024;
const MAX_VALUE_LEN: usize = 16 * 1024 * 1024;
/// Total checked canonical blob size accepted for executable database code.
/// Programs are serialized-pipeline inputs, so they receive a deliberately
/// tighter ceiling than generic durable/index blobs.
pub(crate) const MAX_PROGRAM_BYTES: usize = 4 * 1024 * 1024;
const MAX_COLLECTION_LEN: usize = 1_000_000;
const MAX_PROGRAM_INSTRUCTIONS: usize = 4_096;
const MAX_PROGRAM_BLOCK_DEPTH: usize = 32;
const KIND_TRANSACTION: u8 = 2;
const KIND_REQUEST: u8 = 3;
const KIND_PROGRAM: u8 = 6;
const KIND_PROGRAM_REQUEST: u8 = 7;
const KIND_PROGRAM_OUTPUT: u8 = 8;
const KIND_GENESIS: u8 = 9;
const KIND_SUBMISSION_REQUEST: u8 = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableTransaction {
    pub database_id: String,
    pub basis_t: u64,
    pub previous_hash: Digest,
    /// Exclusive low-42-bit entity-index issuance frontier.
    pub eidx_frontier: u64,
    pub tempids: BTreeMap<String, u64>,
    pub tx_data: Vec<Datom>,
}

pub fn sha256(bytes: &[u8]) -> Digest {
    Sha256::digest(bytes).into()
}

pub fn encode_program(program: &Program) -> Result<Vec<u8>, SemanticError> {
    program.validate()?;
    let mut body = Vec::new();
    body.extend_from_slice(&PROGRAM_ABI_VERSION.to_be_bytes());
    body.push(match program.kind {
        ProgramKind::Transaction => 0,
        ProgramKind::AttributePredicate => 1,
        ProgramKind::Query => 2,
        ProgramKind::EntityPredicate => 3,
        ProgramKind::DualPredicate => 4,
    });
    body.push(program.arity);
    put_len(&mut body, program.instructions.len())?;
    for instruction in &program.instructions {
        encode_instruction(&mut body, instruction)?;
    }
    let encoded = encode_blob(KIND_PROGRAM, &body)?;
    if encoded.len() > MAX_PROGRAM_BYTES {
        return Err(SemanticError::incorrect(
            "program/payload-limit",
            format!("canonical program exceeds the {MAX_PROGRAM_BYTES}-byte limit"),
        ));
    }
    Ok(encoded)
}

pub fn decode_program(bytes: &[u8]) -> Result<Program, SemanticError> {
    // Check before envelope parsing or instruction allocation. A correctly
    // checksummed oversized payload is still invalid executable content.
    if bytes.len() > MAX_PROGRAM_BYTES {
        return Err(fault(
            "encoding/program-payload-limit",
            format!("canonical program exceeds the {MAX_PROGRAM_BYTES}-byte limit"),
        ));
    }
    let body = decode_blob(bytes, KIND_PROGRAM)?;
    let mut cursor = Cursor::new(body);
    let abi_version = cursor.u16()?;
    if abi_version != PROGRAM_ABI_VERSION {
        return Err(fault(
            "encoding/unsupported-program-abi",
            format!("program ABI {abi_version} is unsupported; expected {PROGRAM_ABI_VERSION}"),
        ));
    }
    let kind = match cursor.u8()? {
        0 => ProgramKind::Transaction,
        1 => ProgramKind::AttributePredicate,
        2 => ProgramKind::Query,
        3 => ProgramKind::EntityPredicate,
        4 => ProgramKind::DualPredicate,
        tag => return Err(invalid_tag("program kind", tag)),
    };
    let arity = cursor.u8()?;
    let mut instruction_count = 0usize;
    let instructions = decode_instruction_block(&mut cursor, 0, &mut instruction_count)?;
    cursor.finish()?;
    let program = Program {
        kind,
        arity,
        instructions,
    };
    program.validate().map_err(|error| {
        fault(
            "encoding/invalid-program",
            format!("persisted program failed validation: {error}"),
        )
    })?;
    if encode_program(&program)? != bytes {
        return Err(fault(
            "encoding/noncanonical-program",
            "program payload is not canonical",
        ));
    }
    Ok(program)
}

pub fn program_hash(program: &Program) -> Result<Digest, SemanticError> {
    Ok(sha256(&encode_program(program)?))
}

pub fn program_request_digest(
    hash: ProgramHash,
    arguments: &[Value],
    tx_instant: i64,
    expected_basis_t: u64,
    predicate_hashes: &[ProgramHash],
    source: &EntityRef,
) -> Result<Digest, SemanticError> {
    let mut body = Vec::new();
    body.extend_from_slice(&hash);
    put_u64(&mut body, expected_basis_t);
    put_i64(&mut body, tx_instant);
    encode_entity_ref(&mut body, source)?;
    put_len(&mut body, arguments.len())?;
    for argument in arguments {
        encode_value(&mut body, argument)?;
    }
    let mut predicate_hashes = predicate_hashes.to_vec();
    predicate_hashes.sort();
    predicate_hashes.dedup();
    put_len(&mut body, predicate_hashes.len())?;
    for predicate_hash in predicate_hashes {
        body.extend_from_slice(&predicate_hash);
    }
    Ok(sha256(&encode_blob(KIND_PROGRAM_REQUEST, &body)?))
}

/// Canonical observation bytes for reproducibility checks. Transaction
/// operations and relation rows are unordered, so their element encodings are
/// sorted before the checked envelope is produced.
pub fn encode_program_output(output: &ProgramOutput) -> Result<Vec<u8>, SemanticError> {
    let mut body = Vec::new();
    match output {
        ProgramOutput::Transaction(forms) => {
            body.push(0);
            let mut forms = forms
                .iter()
                .map(|form| {
                    let mut bytes = Vec::new();
                    encode_persistent_tx_form(&mut bytes, form)?;
                    Ok(bytes)
                })
                .collect::<Result<Vec<_>, SemanticError>>()?;
            forms.sort();
            put_len(&mut body, forms.len())?;
            for form in forms {
                put_bytes(&mut body, &form)?;
            }
        }
        ProgramOutput::AttributePredicate(value) => {
            body.push(1);
            encode_runtime_value(&mut body, value, 0)?;
        }
        ProgramOutput::Query(rows) => {
            body.push(2);
            let mut rows = rows
                .iter()
                .map(|row| {
                    let mut bytes = Vec::new();
                    put_len(&mut bytes, row.len())?;
                    for value in row {
                        encode_value(&mut bytes, value)?;
                    }
                    Ok(bytes)
                })
                .collect::<Result<Vec<_>, SemanticError>>()?;
            rows.sort();
            rows.dedup();
            put_len(&mut body, rows.len())?;
            for row in rows {
                put_bytes(&mut body, &row)?;
            }
        }
        ProgramOutput::EntityPredicate(value) => {
            body.push(3);
            encode_runtime_value(&mut body, value, 0)?;
        }
        ProgramOutput::GeneralQuery(rows) => {
            body.push(4);
            let mut rows = rows
                .iter()
                .map(|row| {
                    let mut bytes = Vec::new();
                    put_len(&mut bytes, row.len())?;
                    for value in row {
                        program_query::encode_general_value(&mut bytes, value)?;
                    }
                    Ok(bytes)
                })
                .collect::<Result<Vec<_>, SemanticError>>()?;
            rows.sort();
            rows.dedup();
            put_len(&mut body, rows.len())?;
            for row in rows {
                put_bytes(&mut body, &row)?;
            }
        }
    }
    encode_blob(KIND_PROGRAM_OUTPUT, &body)
}

pub fn encode_transaction(transaction: &DurableTransaction) -> Result<Vec<u8>, SemanticError> {
    validate_transaction(transaction)?;
    let body = transaction_body(transaction)?;
    encode_blob(KIND_TRANSACTION, &body)
}

pub fn decode_transaction(bytes: &[u8]) -> Result<DurableTransaction, SemanticError> {
    let body = decode_blob(bytes, KIND_TRANSACTION)?;
    let mut cursor = Cursor::new(body);
    let database_id = cursor.string()?;
    let basis_t = cursor.u64()?;
    let previous_hash = cursor.digest()?;
    let eidx_frontier = cursor.u64()?;
    let tempid_count = cursor.collection_len()?;
    let mut tempids = BTreeMap::new();
    for _ in 0..tempid_count {
        let name = cursor.string()?;
        let entity = cursor.u64()?;
        if tempids.insert(name, entity).is_some() {
            return Err(fault(
                "encoding/duplicate-tempid",
                "durable transaction contains a duplicate tempid name",
            ));
        }
    }
    let datom_count = cursor.collection_len()?;
    let mut tx_data = Vec::with_capacity(datom_count);
    for _ in 0..datom_count {
        tx_data.push(decode_datom(&mut cursor)?);
    }
    cursor.finish()?;
    let transaction = DurableTransaction {
        database_id,
        basis_t,
        previous_hash,
        eidx_frontier,
        tempids,
        tx_data,
    };
    validate_transaction(&transaction)?;
    if transaction_body(&transaction)? != body {
        return Err(fault(
            "encoding/noncanonical-transaction",
            "transaction payload is not in canonical order or representation",
        ));
    }
    Ok(transaction)
}

pub fn transaction_hash(encoded_transaction: &[u8]) -> Digest {
    sha256(encoded_transaction)
}

/// Hash one datom's canonical typed representation without wrapping it in a
/// size-capped durable blob. State commitments use this per-datom boundary so
/// many large legal values cannot overflow a physical segment limit.
pub(crate) fn canonical_datom_hash(datom: &Datom) -> Result<Digest, SemanticError> {
    let mut encoded = Vec::new();
    encode_datom(&mut encoded, datom)?;
    Ok(sha256(&encoded))
}

/// Canonical typed datom bytes, including valid native genesis assertions.
/// Keeping this codec here prevents a second value representation from
/// drifting away from the authoritative transaction/index encoding.
pub fn canonical_datom_bytes(datom: &Datom) -> Result<Vec<u8>, SemanticError> {
    validate_index_datom_contents(std::slice::from_ref(datom))?;
    let mut encoded = Vec::new();
    encode_datom(&mut encoded, datom)?;
    Ok(encoded)
}

/// Decode exactly `count` concatenated canonical datoms and reject trailing
/// bytes or an allocation-amplifying count before constructing the vector.
pub(crate) fn decode_canonical_datoms(
    bytes: &[u8],
    count: usize,
) -> Result<Vec<Datom>, SemanticError> {
    // The shortest datom is E(8)+A(4)+Bool value(2)+Tx(8)+S(8)+op(1).
    // Bound allocation by authenticated remaining bytes, not an arbitrary
    // item-count policy that would reject otherwise legal content.
    const MIN_ENCODED_DATOM_BYTES: usize = 31;
    if count > bytes.len() / MIN_ENCODED_DATOM_BYTES {
        return Err(fault(
            "encoding/impossible-datom-count",
            "encoded datom count cannot fit in the remaining content bytes",
        ));
    }
    let mut cursor = Cursor::new(bytes);
    let mut datoms = Vec::with_capacity(count);
    for _ in 0..count {
        datoms.push(decode_datom(&mut cursor)?);
    }
    cursor.finish()?;
    Ok(datoms)
}

/// Canonical scalar bytes shared by the durable transaction codec and the
/// persistent index-tree leaf codec. Tree nodes deliberately have their own
/// envelope, kind, and version; only the already-authoritative value
/// representation is shared here.
pub(crate) fn encode_canonical_value(value: &Value) -> Result<Vec<u8>, SemanticError> {
    let mut encoded = Vec::new();
    encode_value(&mut encoded, value)?;
    Ok(encoded)
}

/// Decode exactly one canonical scalar value (without a durable-blob
/// envelope). Rejecting trailing bytes and checking a byte-for-byte re-encode
/// keeps callers from creating a second, looser canonicalization boundary.
pub(crate) fn decode_canonical_value(bytes: &[u8]) -> Result<Value, SemanticError> {
    let mut cursor = Cursor::new(bytes);
    let value = decode_value(&mut cursor, 0)?;
    cursor.finish()?;
    if encode_canonical_value(&value)? != bytes {
        return Err(fault(
            "encoding/noncanonical-value",
            "value payload is not in canonical representation",
        ));
    }
    Ok(value)
}

/// Validate exact datom contents and current index ordering independently of
/// the persistent tree's physical envelope.
pub(crate) fn validate_persistent_index_datoms(
    order: IndexOrder,
    datoms: &[Datom],
) -> Result<(), SemanticError> {
    if datoms
        .windows(2)
        .any(|pair| pair[0].cmp_in(&pair[1], order).is_gt())
    {
        return Err(fault(
            "encoding/unsorted-index-segment",
            "persistent index datoms must use the current tree ordering",
        ));
    }
    validate_index_datom_contents(datoms)
}

/// Encode the authoritative t=0 information set. Genesis has its own checked
/// kind because ordinary durable transactions must always have positive t.
pub fn encode_genesis(datoms: &[Datom]) -> Result<Vec<u8>, SemanticError> {
    validate_genesis(datoms)?;
    let mut body = Vec::new();
    put_len(&mut body, datoms.len())?;
    for datom in datoms {
        encode_datom(&mut body, datom)?;
    }
    encode_blob(KIND_GENESIS, &body)
}

pub fn decode_genesis(bytes: &[u8]) -> Result<Vec<Datom>, SemanticError> {
    let body = decode_blob(bytes, KIND_GENESIS)?;
    let mut cursor = Cursor::new(body);
    let count = cursor.collection_len()?;
    let mut datoms = Vec::with_capacity(count);
    for _ in 0..count {
        datoms.push(decode_datom(&mut cursor)?);
    }
    cursor.finish()?;
    validate_genesis(&datoms)?;
    if encode_genesis(&datoms)? != bytes {
        return Err(fault(
            "encoding/noncanonical-genesis",
            "genesis payload is not canonical",
        ));
    }
    Ok(datoms)
}

fn validate_index_datom_contents(datoms: &[Datom]) -> Result<(), SemanticError> {
    for datom in datoms {
        validate_supported_eid(datom.source).map_err(|error| {
            fault(
                "encoding/invalid-datom-source",
                format!("datom source id is invalid: {error}"),
            )
        })?;
        let datom_t = tx_to_t(datom.tx).map_err(|error| {
            fault(
                "encoding/invalid-index-datom-transaction",
                format!("index datom transaction is invalid: {error}"),
            )
        })?;
        if datom_t == 0 {
            if !datom.added {
                return Err(fault(
                    "encoding/invalid-index-genesis-datom",
                    "t=0 index datoms must be genesis assertions",
                ));
            }
            validate_supported_eid(datom.entity).map_err(|error| {
                fault(
                    "encoding/invalid-index-datom-entity",
                    format!("index datom entity id is invalid: {error}"),
                )
            })?;
            validate_encoded_value_refs(&datom.value)?;
        } else {
            validate_encoded_datom(datom)?;
        }
    }
    Ok(())
}

/// Comparator used by current EDBC genesis and transaction values. The native tree comparator intentionally
/// differs: it places descending T/op before the stored representation tie.
fn format_datom_cmp(left: &Datom, right: &Datom, order: IndexOrder) -> std::cmp::Ordering {
    let ordering = match order {
        IndexOrder::Eavt => left
            .entity
            .cmp(&right.entity)
            .then(left.attribute.cmp(&right.attribute))
            .then_with(|| format_value_cmp(&left.value, &right.value)),
        IndexOrder::Aevt => left
            .attribute
            .cmp(&right.attribute)
            .then(left.entity.cmp(&right.entity))
            .then_with(|| format_value_cmp(&left.value, &right.value)),
        IndexOrder::Avet => left
            .attribute
            .cmp(&right.attribute)
            .then_with(|| format_value_cmp(&left.value, &right.value))
            .then(left.entity.cmp(&right.entity)),
        IndexOrder::Vaet => format_value_cmp(&left.value, &right.value)
            .then(left.attribute.cmp(&right.attribute))
            .then(left.entity.cmp(&right.entity)),
        IndexOrder::Saevt => left
            .source
            .cmp(&right.source)
            .then(left.attribute.cmp(&right.attribute))
            .then(left.entity.cmp(&right.entity))
            .then_with(|| format_value_cmp(&left.value, &right.value)),
        IndexOrder::Savet => left
            .source
            .cmp(&right.source)
            .then(left.attribute.cmp(&right.attribute))
            .then_with(|| format_value_cmp(&left.value, &right.value))
            .then(left.entity.cmp(&right.entity)),
    };
    ordering
        .then_with(|| right.tx.cmp(&left.tx))
        .then(left.source.cmp(&right.source))
        .then_with(|| right.added.cmp(&left.added))
}

fn format_value_cmp(left: &Value, right: &Value) -> std::cmp::Ordering {
    left.index_cmp(right).then_with(|| match (left, right) {
        (Value::BigDec(left), Value::BigDec(right)) => left
            .fractional_digit_count()
            .cmp(&right.fractional_digit_count()),
        (Value::Tuple(left), Value::Tuple(right)) => left
            .iter()
            .zip(right)
            .find_map(|(left, right)| {
                let ordering = match (left, right) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Less,
                    (Some(_), None) => std::cmp::Ordering::Greater,
                    (Some(left), Some(right)) => format_value_cmp(left, right),
                };
                ordering.is_ne().then_some(ordering)
            })
            .unwrap_or_else(|| left.len().cmp(&right.len())),
        _ => std::cmp::Ordering::Equal,
    })
}

/// Canonical digest of an unordered primitive transaction request.
pub fn request_digest(
    ops: &[TxOp],
    tx_instant: i64,
    expected_basis_t: u64,
    source: &EntityRef,
) -> Result<Digest, SemanticError> {
    let mut encoded_ops = Vec::with_capacity(ops.len());
    for op in ops {
        let mut bytes = Vec::new();
        encode_tx_op(&mut bytes, op)?;
        encoded_ops.push(bytes);
    }
    encoded_ops.sort();
    let mut body = Vec::new();
    put_u64(&mut body, expected_basis_t);
    put_i64(&mut body, tx_instant);
    encode_entity_ref(&mut body, source)?;
    put_len(&mut body, encoded_ops.len())?;
    for encoded in encoded_ops {
        put_framed_bytes(&mut body, &encoded)?;
    }
    Ok(sha256(&encode_blob(KIND_REQUEST, &body)?))
}

/// Canonical identity of an authoritative declarative submission.
///
/// The basis and instant are absent for an ordinary request because the
/// transactor selects both after locking the actual db-before.  Explicit
/// compare-basis and import-time constraints remain part of request identity.
pub fn submission_request_digest(
    forms: &[TxForm],
    source: &EntityRef,
    compare_basis_t: Option<u64>,
    tx_instant_override: Option<i64>,
) -> Result<Digest, SemanticError> {
    Ok(canonical_submission_request(
        forms,
        source,
        compare_basis_t,
        tx_instant_override,
        MAX_BLOB_LEN + HEADER_LEN + CHECKSUM_LEN,
    )?
    .0)
}

/// Encode an authoritative declarative submission once before queue
/// admission, returning its stable digest and exact canonical byte footprint.
/// Dynamic db-before, selected time, and resolved function hashes are absent.
pub(crate) fn canonical_submission_request(
    forms: &[TxForm],
    source: &EntityRef,
    compare_basis_t: Option<u64>,
    tx_instant_override: Option<i64>,
    max_bytes: usize,
) -> Result<(Digest, usize), SemanticError> {
    let mut encoded_forms = Vec::with_capacity(forms.len());
    let mut form_bytes = 0usize;
    for form in forms {
        let mut bytes = Vec::new();
        encode_persistent_tx_form(&mut bytes, form)?;
        form_bytes = form_bytes
            .checked_add(bytes.len().saturating_add(4))
            .ok_or_else(|| submission_too_large(max_bytes))?;
        if form_bytes > max_bytes {
            return Err(submission_too_large(max_bytes));
        }
        encoded_forms.push(bytes);
    }
    encoded_forms.sort();

    let mut body = Vec::new();
    // One current identity grammar for every supported transaction form.
    body.push(submission::VERSION);
    encode_entity_ref(&mut body, source)?;
    match compare_basis_t {
        Some(basis) => {
            body.push(1);
            put_u64(&mut body, basis);
        }
        None => body.push(0),
    }
    match tx_instant_override {
        Some(instant) => {
            body.push(1);
            put_i64(&mut body, instant);
        }
        None => body.push(0),
    }
    put_len(&mut body, encoded_forms.len())?;
    for encoded in encoded_forms {
        put_framed_bytes(&mut body, &encoded)?;
    }
    let encoded = encode_blob(KIND_SUBMISSION_REQUEST, &body)?;
    if encoded.len() > max_bytes {
        return Err(submission_too_large(max_bytes));
    }
    Ok((sha256(&encoded), encoded.len()))
}

fn submission_too_large(max_bytes: usize) -> SemanticError {
    SemanticError::new(
        ErrorCategory::Busy,
        "service/request-byte-capacity",
        format!("canonical transaction request exceeds the {max_bytes}-byte admission limit"),
    )
}

fn transaction_body(transaction: &DurableTransaction) -> Result<Vec<u8>, SemanticError> {
    let mut datoms = transaction
        .tx_data
        .iter()
        .map(|datom| {
            let mut encoded = Vec::new();
            encode_datom(&mut encoded, datom)?;
            Ok((datom, encoded))
        })
        .collect::<Result<Vec<_>, SemanticError>>()?;
    datoms.sort_by(|(left, left_bytes), (right, right_bytes)| {
        format_datom_cmp(left, right, crate::IndexOrder::Eavt)
            .then_with(|| left_bytes.cmp(right_bytes))
    });
    let mut body = Vec::new();
    put_string(&mut body, &transaction.database_id)?;
    put_u64(&mut body, transaction.basis_t);
    body.extend_from_slice(&transaction.previous_hash);
    put_u64(&mut body, transaction.eidx_frontier);
    put_len(&mut body, transaction.tempids.len())?;
    for (name, entity) in &transaction.tempids {
        put_string(&mut body, name)?;
        put_u64(&mut body, *entity);
    }
    put_len(&mut body, datoms.len())?;
    for (_, encoded) in datoms {
        body.extend_from_slice(&encoded);
    }
    Ok(body)
}

fn validate_transaction(transaction: &DurableTransaction) -> Result<(), SemanticError> {
    if transaction.database_id.is_empty() {
        return Err(fault(
            "encoding/empty-database-id",
            "durable transaction database id cannot be empty",
        ));
    }
    let tx = validate_transaction_content(
        transaction.basis_t,
        transaction.eidx_frontier,
        &transaction.tx_data,
    )?;
    for entity in transaction.tempids.values().copied() {
        validate_issued_entity(
            entity,
            transaction.eidx_frontier,
            transaction.basis_t,
            "tempid",
        )?;
    }
    debug_assert_eq!(tx, t_to_tx(transaction.basis_t).expect("validated basis"));
    Ok(())
}

/// Validate the database-information portion shared by transaction
/// envelopes and lineage content without manufacturing a tempid-name map.
pub(crate) fn validate_transaction_content(
    basis_t: u64,
    eidx_frontier: u64,
    tx_data: &[Datom],
) -> Result<u64, SemanticError> {
    if basis_t == 0 {
        return Err(fault(
            "encoding/invalid-basis",
            "durable transaction basis must be positive",
        ));
    }
    let tx = t_to_tx(basis_t).map_err(|error| {
        fault(
            "encoding/basis-out-of-range",
            format!("durable transaction basis cannot be represented: {error}"),
        )
    })?;
    validate_frontier(eidx_frontier).map_err(|error| {
        fault(
            "encoding/invalid-issued-frontier",
            format!("durable transaction has an invalid issued frontier: {error}"),
        )
    })?;
    if tx_data.iter().any(|datom| datom.tx != tx) {
        return Err(fault(
            "encoding/datom-transaction-mismatch",
            "every datom must name the transaction entity for the envelope basis",
        ));
    }
    if tx_data
        .first()
        .is_some_and(|first| tx_data.iter().any(|datom| datom.source != first.source))
    {
        return Err(fault(
            "encoding/mixed-transaction-source",
            "every datom in a transaction must have the same source",
        ));
    }
    for datom in tx_data {
        validate_issued_entity(datom.source, eidx_frontier, basis_t, "source")?;
        validate_encoded_datom(datom)?;
        validate_issued_entity(datom.entity, eidx_frontier, basis_t, "datom")?;
        validate_issued_value_refs(&datom.value, eidx_frontier, basis_t)?;
        let datom_t = tx_to_t(datom.tx).expect("transaction equality was checked");
        if datom_t > MAX_EIDX {
            return Err(fault(
                "encoding/datom-transaction-out-of-range",
                "datom transaction time exceeds the recovered entity-index width",
            ));
        }
    }
    // Sort only borrowed references: O(n) pointers and O(n log n) comparisons,
    // without copying values or changing the canonical transaction order.
    // Do not sort by stored_cmp: top-level decimal scale makes stored equality
    // non-transitive across representations (1.0M != 1.00M, but both equal 1).
    // Logical groups are ordered first; non-decimals precede decimals, whose
    // scales then order strictly. A group with a non-decimal and another value
    // necessarily has an adjacent forbidden pair. An all-decimal group has one
    // exactly when a scale repeats. Tuples use logical recursive equality, not
    // the top-level decimal scale distinction, so need no extra tie-break.
    let mut ordered: Vec<&Datom> = tx_data.iter().collect();
    ordered.sort_unstable_by(|left, right| {
        left.entity
            .cmp(&right.entity)
            .then_with(|| left.attribute.cmp(&right.attribute))
            .then_with(|| left.value.index_cmp(&right.value))
            .then_with(|| match (&left.value, &right.value) {
                (Value::BigDec(left), Value::BigDec(right)) => left
                    .fractional_digit_count()
                    .cmp(&right.fractional_digit_count()),
                (Value::BigDec(_), _) => std::cmp::Ordering::Greater,
                (_, Value::BigDec(_)) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Equal,
            })
    });
    if ordered.windows(2).any(|pair| {
        pair[0].entity == pair[1].entity
            && pair[0].attribute == pair[1].attribute
            && pair[0].value.stored_eq(&pair[1].value)
    }) {
        return Err(fault(
            "encoding/duplicate-datom",
            "durable transaction contains duplicate or contradictory datoms",
        ));
    }
    Ok(tx)
}

fn validate_genesis(datoms: &[Datom]) -> Result<(), SemanticError> {
    let tx = t_to_tx(0).expect("genesis t is representable");
    if datoms.is_empty() {
        return Err(fault(
            "encoding/empty-genesis",
            "genesis information cannot be empty",
        ));
    }
    if datoms.iter().any(|datom| {
        datom.tx != tx || !datom.added || datom.source != crate::model::vocabulary::DB_SOURCE_SYSTEM
    }) {
        return Err(fault(
            "encoding/invalid-genesis-datom",
            "genesis contains only system-attributed t=0 assertions",
        ));
    }
    if datoms
        .windows(2)
        .any(|pair| !format_datom_cmp(&pair[0], &pair[1], IndexOrder::Eavt).is_lt())
    {
        return Err(fault(
            "encoding/noncanonical-genesis",
            "genesis datoms must be strictly ordered in EAVT order",
        ));
    }
    for datom in datoms {
        validate_supported_eid(datom.entity).map_err(|error| {
            fault(
                "encoding/invalid-genesis-entity",
                format!("genesis entity id is invalid: {error}"),
            )
        })?;
    }
    Ok(())
}

fn validate_encoded_datom(datom: &Datom) -> Result<(), SemanticError> {
    validate_supported_eid(datom.source).map_err(|error| {
        fault(
            "encoding/invalid-datom-source",
            format!("datom source id is invalid: {error}"),
        )
    })?;
    validate_supported_eid(datom.entity).map_err(|error| {
        fault(
            "encoding/invalid-datom-entity",
            format!("datom entity id is invalid: {error}"),
        )
    })?;
    let t = tx_to_t(datom.tx).map_err(|error| {
        fault(
            "encoding/invalid-datom-transaction",
            format!("datom transaction is not a transaction entity id: {error}"),
        )
    })?;
    if t == 0 {
        return Err(fault(
            "encoding/invalid-datom-transaction",
            "persisted datoms must belong to a positive transaction",
        ));
    }
    validate_encoded_value_refs(&datom.value)
}

fn validate_encoded_value_refs(value: &Value) -> Result<(), SemanticError> {
    match value {
        Value::Ref(entity) => validate_supported_eid(*entity).map_err(|error| {
            fault(
                "encoding/invalid-ref-entity",
                format!("reference value contains an invalid entity id: {error}"),
            )
        }),
        Value::Tuple(slots) => {
            for value in slots.iter().flatten() {
                validate_encoded_value_refs(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_issued_entity(
    entity: u64,
    frontier: u64,
    basis_t: u64,
    context: &str,
) -> Result<(), SemanticError> {
    validate_supported_eid(entity).map_err(|error| {
        fault(
            "encoding/invalid-issued-entity",
            format!("{context} contains an invalid entity id: {error}"),
        )
    })?;
    let eidx = eid_to_eidx(entity).expect("supported entity id was checked");
    if eidx >= frontier {
        return Err(fault(
            "encoding/unissued-entity",
            format!("{context} entity index {eidx} is not below issued frontier {frontier}"),
        ));
    }
    if crate::eid_to_part(entity).expect("supported entity id was checked") == crate::TX_PARTITION {
        let t = tx_to_t(entity).expect("transaction partition was checked");
        if t == 0 || t > basis_t {
            return Err(fault(
                "encoding/transaction-entity-out-of-range",
                format!("{context} references transaction t={t} beyond basis {basis_t}"),
            ));
        }
    }
    Ok(())
}

fn validate_issued_value_refs(
    value: &Value,
    frontier: u64,
    basis_t: u64,
) -> Result<(), SemanticError> {
    match value {
        Value::Ref(entity) => validate_issued_entity(*entity, frontier, basis_t, "reference"),
        Value::Tuple(slots) => {
            for value in slots.iter().flatten() {
                validate_issued_value_refs(value, frontier, basis_t)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn encode_blob(kind: u8, body: &[u8]) -> Result<Vec<u8>, SemanticError> {
    if body.len() > MAX_BLOB_LEN {
        return Err(SemanticError::incorrect(
            "encoding/blob-too-large",
            format!("encoded body exceeds {MAX_BLOB_LEN} bytes"),
        ));
    }
    let mut bytes = Vec::with_capacity(HEADER_LEN + body.len() + CHECKSUM_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.push(kind);
    bytes.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&(body.len() as u64).to_be_bytes());
    bytes.extend_from_slice(body);
    let checksum = sha256(&bytes);
    bytes.extend_from_slice(&checksum);
    Ok(bytes)
}

fn decode_blob(bytes: &[u8], expected_kind: u8) -> Result<&[u8], SemanticError> {
    if bytes.len() < HEADER_LEN + CHECKSUM_LEN {
        return Err(fault(
            "encoding/truncated-blob",
            "durable blob is shorter than its header and checksum",
        ));
    }
    if &bytes[..4] != MAGIC {
        return Err(fault("encoding/bad-magic", "durable blob magic is invalid"));
    }
    if bytes[4] != expected_kind {
        return Err(fault(
            "encoding/unexpected-kind",
            format!("expected blob kind {expected_kind}, got {}", bytes[4]),
        ));
    }
    let version = u16::from_be_bytes([bytes[5], bytes[6]]);
    if version != FORMAT_VERSION {
        return Err(SemanticError::new(
            ErrorCategory::Unsupported,
            "encoding/unsupported-version",
            format!("unsupported durable format version {version}"),
        ));
    }
    if bytes[7] != 0 {
        return Err(fault(
            "encoding/nonzero-reserved",
            "durable blob reserved header byte must be zero",
        ));
    }
    let body_len = u64::from_be_bytes(bytes[8..16].try_into().expect("fixed header slice"));
    let body_len = usize::try_from(body_len).map_err(|_| {
        fault(
            "encoding/blob-too-large",
            "durable blob length does not fit this platform",
        )
    })?;
    if body_len > MAX_BLOB_LEN {
        return Err(fault(
            "encoding/blob-too-large",
            format!("durable blob declares more than {MAX_BLOB_LEN} bytes"),
        ));
    }
    let expected_len = HEADER_LEN
        .checked_add(body_len)
        .and_then(|length| length.checked_add(CHECKSUM_LEN))
        .ok_or_else(|| fault("encoding/blob-too-large", "durable blob length overflow"))?;
    if bytes.len() != expected_len {
        return Err(fault(
            "encoding/length-mismatch",
            "durable blob length does not match its header",
        ));
    }
    let checksum_at = HEADER_LEN + body_len;
    if sha256(&bytes[..checksum_at]).as_slice() != &bytes[checksum_at..] {
        return Err(fault(
            "encoding/checksum-mismatch",
            "durable blob checksum does not match its contents",
        ));
    }
    Ok(&bytes[HEADER_LEN..checksum_at])
}

fn encode_instruction(
    output: &mut Vec<u8>,
    instruction: &Instruction,
) -> Result<(), SemanticError> {
    match instruction {
        Instruction::PushArgument(index) => {
            output.push(0);
            output.push(*index);
        }
        Instruction::PushConstant(value) => {
            output.push(1);
            encode_value(output, value)?;
        }
        Instruction::PushEntity(entity) => {
            output.push(22);
            encode_entity_ref(output, entity)?;
        }
        Instruction::PushNull => output.push(23),
        Instruction::Duplicate => output.push(2),
        Instruction::Pop => output.push(3),
        Instruction::Swap => output.push(24),
        Instruction::MakeVector(width) => {
            output.push(25);
            output.push(*width);
        }
        Instruction::MakeMap(pair_count) => {
            output.push(26);
            output.push(*pair_count);
        }
        Instruction::Get => output.push(27),
        Instruction::ContainsKey => output.push(28),
        Instruction::Length => output.push(29),
        Instruction::Unpack(width) => {
            output.push(30);
            output.push(*width);
        }
        Instruction::If {
            then_branch,
            else_branch,
        } => {
            output.push(31);
            encode_instruction_block(output, then_branch)?;
            encode_instruction_block(output, else_branch)?;
        }
        Instruction::PredicateDispatch { attribute, entity } => {
            output.push(41);
            encode_instruction_block(output, attribute)?;
            encode_instruction_block(output, entity)?;
        }
        Instruction::ForEach { body } => {
            output.push(32);
            encode_instruction_block(output, body)?;
        }
        Instruction::LoadOne(attribute) => {
            output.push(4);
            put_u32(output, *attribute);
        }
        Instruction::Exists(attribute) => {
            output.push(5);
            put_u32(output, *attribute);
        }
        Instruction::LoadMany(attribute) => {
            output.push(33);
            put_u32(output, *attribute);
        }
        Instruction::Query(template) => {
            output.push(37);
            encode_query_template(output, template)?;
        }
        Instruction::Add => output.push(6),
        Instruction::Subtract => output.push(7),
        Instruction::Multiply => output.push(8),
        Instruction::Divide => output.push(9),
        Instruction::Equal => output.push(10),
        Instruction::LessThan => output.push(11),
        Instruction::GreaterThan => output.push(12),
        Instruction::Not => output.push(13),
        Instruction::And => output.push(14),
        Instruction::Or => output.push(15),
        Instruction::Require { category, message } => {
            output.push(16);
            output.push(match category {
                ErrorCategory::Incorrect => 0,
                ErrorCategory::Conflict => 2,
                _ => {
                    return Err(SemanticError::incorrect(
                        "program/rejection-category",
                        "program rejection category must be incorrect or conflict",
                    ));
                }
            });
            put_string(output, message)?;
        }
        Instruction::RequireAnomaly => output.push(40),
        Instruction::EmitAdd(attribute) => {
            output.push(17);
            put_u32(output, *attribute);
        }
        Instruction::EmitRetract(attribute) => {
            output.push(18);
            put_u32(output, *attribute);
        }
        Instruction::EmitRetractAll(attribute) => {
            output.push(38);
            put_u32(output, *attribute);
        }
        Instruction::EmitCas(attribute) => {
            output.push(34);
            put_u32(output, *attribute);
        }
        Instruction::EmitRetractEntity => output.push(35),
        Instruction::EmitEnsure => output.push(36),
        Instruction::EmitForcePartition => output.push(42),
        Instruction::EmitMatchPartition => output.push(43),
        Instruction::EmitEntityMap => output.push(39),
        Instruction::Return => output.push(19),
        Instruction::EmitRow(width) => {
            output.push(20);
            output.push(*width);
        }
        Instruction::EmitCall {
            function,
            argument_count,
        } => {
            output.push(21);
            match function {
                CallableRef::Database(entity) => {
                    output.push(0);
                    encode_entity_ref(output, entity)?;
                }
                CallableRef::ExactHash(hash) => {
                    output.push(1);
                    output.extend_from_slice(hash);
                }
                CallableRef::Local(symbol) => {
                    output.push(2);
                    encode_symbol(output, symbol)?;
                }
            }
            output.push(*argument_count);
        }
    }
    Ok(())
}

fn decode_instruction(
    cursor: &mut Cursor<'_>,
    block_depth: usize,
    instruction_count: &mut usize,
) -> Result<Instruction, SemanticError> {
    *instruction_count = instruction_count.checked_add(1).ok_or_else(|| {
        fault(
            "encoding/program-instruction-limit",
            "program instruction count overflowed",
        )
    })?;
    if *instruction_count > MAX_PROGRAM_INSTRUCTIONS {
        return Err(fault(
            "encoding/program-instruction-limit",
            format!("program contains more than {MAX_PROGRAM_INSTRUCTIONS} instructions"),
        ));
    }
    Ok(match cursor.u8()? {
        0 => Instruction::PushArgument(cursor.u8()?),
        1 => Instruction::PushConstant(decode_value(cursor, 0)?),
        2 => Instruction::Duplicate,
        3 => Instruction::Pop,
        4 => Instruction::LoadOne(cursor.u32()?),
        5 => Instruction::Exists(cursor.u32()?),
        6 => Instruction::Add,
        7 => Instruction::Subtract,
        8 => Instruction::Multiply,
        9 => Instruction::Divide,
        10 => Instruction::Equal,
        11 => Instruction::LessThan,
        12 => Instruction::GreaterThan,
        13 => Instruction::Not,
        14 => Instruction::And,
        15 => Instruction::Or,
        16 => {
            let category = match cursor.u8()? {
                0 => ErrorCategory::Incorrect,
                2 => ErrorCategory::Conflict,
                tag => return Err(invalid_tag("program rejection category", tag)),
            };
            Instruction::Require {
                category,
                message: cursor.string()?,
            }
        }
        17 => Instruction::EmitAdd(cursor.u32()?),
        18 => Instruction::EmitRetract(cursor.u32()?),
        38 => Instruction::EmitRetractAll(cursor.u32()?),
        19 => Instruction::Return,
        20 => Instruction::EmitRow(cursor.u8()?),
        21 => {
            let function = match cursor.u8()? {
                0 => CallableRef::Database(decode_entity_ref(cursor)?),
                1 => CallableRef::ExactHash(cursor.digest()?),
                2 => CallableRef::Local(decode_symbol(cursor)?),
                tag => return Err(invalid_tag("program callable", tag)),
            };
            Instruction::EmitCall {
                function,
                argument_count: cursor.u8()?,
            }
        }
        22 => Instruction::PushEntity(decode_entity_ref(cursor)?),
        23 => Instruction::PushNull,
        24 => Instruction::Swap,
        25 => Instruction::MakeVector(cursor.u8()?),
        26 => Instruction::MakeMap(cursor.u8()?),
        27 => Instruction::Get,
        28 => Instruction::ContainsKey,
        29 => Instruction::Length,
        30 => Instruction::Unpack(cursor.u8()?),
        31 => Instruction::If {
            then_branch: decode_instruction_block(cursor, block_depth + 1, instruction_count)?,
            else_branch: decode_instruction_block(cursor, block_depth + 1, instruction_count)?,
        },
        41 => Instruction::PredicateDispatch {
            attribute: decode_instruction_block(cursor, block_depth, instruction_count)?,
            entity: decode_instruction_block(cursor, block_depth, instruction_count)?,
        },
        32 => Instruction::ForEach {
            body: decode_instruction_block(cursor, block_depth + 1, instruction_count)?,
        },
        33 => Instruction::LoadMany(cursor.u32()?),
        34 => Instruction::EmitCas(cursor.u32()?),
        35 => Instruction::EmitRetractEntity,
        36 => Instruction::EmitEnsure,
        42 => Instruction::EmitForcePartition,
        43 => Instruction::EmitMatchPartition,
        37 => Instruction::Query(decode_query_template(cursor)?),
        39 => Instruction::EmitEntityMap,
        40 => Instruction::RequireAnomaly,
        tag => return Err(invalid_tag("program instruction", tag)),
    })
}

fn encode_instruction_block(
    output: &mut Vec<u8>,
    instructions: &[Instruction],
) -> Result<(), SemanticError> {
    put_len(output, instructions.len())?;
    for instruction in instructions {
        encode_instruction(output, instruction)?;
    }
    Ok(())
}

fn decode_instruction_block(
    cursor: &mut Cursor<'_>,
    block_depth: usize,
    instruction_count: &mut usize,
) -> Result<Vec<Instruction>, SemanticError> {
    if block_depth > MAX_PROGRAM_BLOCK_DEPTH {
        return Err(fault(
            "encoding/program-block-depth",
            format!("program blocks nest beyond {MAX_PROGRAM_BLOCK_DEPTH} levels"),
        ));
    }
    let count = cursor.collection_len()?;
    if count > MAX_PROGRAM_INSTRUCTIONS.saturating_sub(*instruction_count) {
        return Err(fault(
            "encoding/program-instruction-limit",
            format!("program contains more than {MAX_PROGRAM_INSTRUCTIONS} instructions"),
        ));
    }
    let mut instructions = Vec::with_capacity(count);
    for _ in 0..count {
        instructions.push(decode_instruction(cursor, block_depth, instruction_count)?);
    }
    Ok(instructions)
}

fn encode_query_template(
    output: &mut Vec<u8>,
    template: &QueryTemplate,
) -> Result<(), SemanticError> {
    put_bool(output, template.canonical_order);
    program_query::encode_template(output, template.native_spec())
}

fn decode_query_template(cursor: &mut Cursor<'_>) -> Result<QueryTemplate, SemanticError> {
    let canonical_order = cursor.boolean()?;
    let mut template = program_query::decode_template(cursor)?;
    template.canonical_order = canonical_order;
    Ok(template)
}

fn encode_datom(output: &mut Vec<u8>, datom: &Datom) -> Result<(), SemanticError> {
    put_u64(output, datom.entity);
    put_u32(output, datom.attribute);
    encode_value(output, &datom.value)?;
    put_u64(output, datom.tx);
    put_u64(output, datom.source);
    output.push(u8::from(datom.added));
    Ok(())
}

fn decode_datom(cursor: &mut Cursor<'_>) -> Result<Datom, SemanticError> {
    let entity = cursor.u64()?;
    let attribute = cursor.u32()?;
    let value = decode_value(cursor, 0)?;
    let tx = cursor.u64()?;
    let source = cursor.u64()?;
    let added = cursor.boolean()?;
    Ok(Datom {
        entity,
        attribute,
        value,
        tx,
        source,
        added,
    })
}

fn encode_attribute(output: &mut Vec<u8>, attribute: &Attribute) -> Result<(), SemanticError> {
    put_u32(output, attribute.id);
    encode_keyword(output, &attribute.ident)?;
    output.push(value_type_tag(attribute.value_type));
    output.push(match attribute.cardinality {
        Cardinality::One => 0,
        Cardinality::Many => 1,
    });
    output.push(match attribute.unique {
        None => 0,
        Some(Unique::Identity) => 1,
        Some(Unique::Value) => 2,
    });
    put_bool(output, attribute.indexed);
    put_bool(output, attribute.component);
    put_bool(output, attribute.no_history);
    put_bool(output, attribute.fulltext);
    match &attribute.tuple {
        None => output.push(0),
        Some(TupleSpec::Homogeneous(value_type)) => {
            output.push(1);
            output.push(value_type_tag(*value_type));
        }
        Some(TupleSpec::Heterogeneous(types)) => {
            output.push(2);
            put_len(output, types.len())?;
            for value_type in types {
                output.push(value_type_tag(*value_type));
            }
        }
        Some(TupleSpec::Composite(attributes)) => {
            output.push(3);
            put_len(output, attributes.len())?;
            for attribute in attributes {
                put_u32(output, *attribute);
            }
        }
    }
    put_bool(output, attribute.tuple_discontinued);
    let mut predicates = attribute.predicates.clone();
    predicates.sort();
    predicates.dedup();
    put_len(output, predicates.len())?;
    for predicate in predicates {
        put_string(output, &predicate)?;
    }
    Ok(())
}

fn encode_value(output: &mut Vec<u8>, value: &Value) -> Result<(), SemanticError> {
    crate::transaction::input::validate_stored_input(value)?;
    match value {
        Value::BigDec(value) => {
            output.push(0);
            let (integer, scale) = value.as_bigint_and_exponent();
            put_i64(output, scale);
            put_bigint(output, &integer)?;
        }
        Value::BigInt(value) => {
            output.push(1);
            put_bigint(output, value)?;
        }
        Value::Bool(value) => {
            output.push(2);
            put_bool(output, *value);
        }
        Value::Bytes(value) => {
            output.push(3);
            put_bytes(output, value)?;
        }
        Value::Double(value) => {
            output.push(4);
            put_u64(output, canonical_f64_bits(*value));
        }
        Value::Float(value) => {
            output.push(5);
            put_u32(output, canonical_f32_bits(*value));
        }
        Value::Function(hash) => {
            // Function values are immutable native program identities. The
            // canonical payload is the content hash itself, never JVM code.
            output.push(15);
            output.extend_from_slice(hash);
        }
        Value::Instant(value) => {
            output.push(6);
            put_i64(output, *value);
        }
        Value::Keyword(value) => {
            output.push(7);
            encode_keyword(output, value)?;
        }
        Value::Long(value) => {
            output.push(8);
            put_i64(output, *value);
        }
        Value::Ref(value) => {
            output.push(9);
            put_u64(output, *value);
        }
        Value::String(value) => {
            output.push(10);
            put_string(output, value)?;
        }
        Value::Symbol(value) => {
            output.push(11);
            encode_symbol(output, value)?;
        }
        Value::Tuple(values) => {
            output.push(12);
            put_len(output, values.len())?;
            for value in values {
                match value {
                    None => output.push(0),
                    Some(value) => {
                        output.push(1);
                        encode_value(output, value)?;
                    }
                }
            }
        }
        Value::Uuid(value) => {
            output.push(13);
            output.extend_from_slice(&value.to_be_bytes());
        }
        Value::Uri(value) => {
            output.push(14);
            put_string(output, value)?;
        }
    }
    Ok(())
}

fn decode_value(cursor: &mut Cursor<'_>, depth: usize) -> Result<Value, SemanticError> {
    if depth > 16 {
        return Err(fault(
            "encoding/value-depth",
            "encoded value nesting exceeds 16 levels",
        ));
    }
    match cursor.u8()? {
        0 => {
            let scale = cursor.i64()?;
            Ok(Value::BigDec(BigDecimal::new(cursor.bigint()?, scale)))
        }
        1 => Ok(Value::BigInt(cursor.bigint()?)),
        2 => Ok(Value::Bool(cursor.boolean()?)),
        3 => Ok(Value::Bytes(cursor.bytes()?.to_vec())),
        4 => {
            let bits = cursor.u64()?;
            let value = f64::from_bits(bits);
            if bits != canonical_f64_bits(value) {
                return Err(fault(
                    "encoding/noncanonical-float",
                    "double uses a noncanonical zero or NaN representation",
                ));
            }
            Ok(Value::Double(value))
        }
        5 => {
            let bits = cursor.u32()?;
            let value = f32::from_bits(bits);
            if bits != canonical_f32_bits(value) {
                return Err(fault(
                    "encoding/noncanonical-float",
                    "float uses a noncanonical zero or NaN representation",
                ));
            }
            Ok(Value::Float(value))
        }
        6 => Ok(Value::Instant(cursor.i64()?)),
        7 => Ok(Value::Keyword(decode_keyword(cursor)?)),
        8 => Ok(Value::Long(cursor.i64()?)),
        9 => Ok(Value::Ref(cursor.u64()?)),
        10 => Ok(Value::String(cursor.string()?)),
        11 => Ok(Value::Symbol(decode_symbol(cursor)?)),
        12 => {
            let count = cursor.collection_len()?;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(match cursor.u8()? {
                    0 => None,
                    1 => Some(decode_value(cursor, depth + 1)?),
                    tag => return Err(invalid_tag("tuple slot", tag)),
                });
            }
            Ok(Value::Tuple(values))
        }
        13 => Ok(Value::Uuid(u128::from_be_bytes(cursor.array()?))),
        14 => {
            let uri = cursor.string()?;
            if !crate::model::uri::validate(&uri) {
                return Err(fault(
                    "encoding/invalid-uri",
                    "encoded URI has invalid syntax or escaping",
                ));
            }
            Ok(Value::Uri(uri))
        }
        15 => Ok(Value::Function(cursor.array()?)),
        tag => Err(invalid_tag("value", tag)),
    }
}

fn encode_tx_op(output: &mut Vec<u8>, op: &TxOp) -> Result<(), SemanticError> {
    match op {
        TxOp::Add {
            entity,
            attribute,
            value,
        } => {
            output.push(0);
            encode_entity_ref(output, entity)?;
            put_u32(output, *attribute);
            encode_tx_value(output, value)?;
        }
        TxOp::Retract {
            entity,
            attribute,
            value,
        } => {
            output.push(1);
            encode_entity_ref(output, entity)?;
            put_u32(output, *attribute);
            match value {
                None => output.push(0),
                Some(value) => {
                    output.push(1);
                    encode_tx_value(output, value)?;
                }
            }
        }
        TxOp::Cas {
            entity,
            attribute,
            old,
            new,
        } => {
            output.push(2);
            encode_entity_ref(output, entity)?;
            put_u32(output, *attribute);
            match old {
                None => output.push(0),
                Some(value) => {
                    output.push(1);
                    encode_tx_value(output, value)?;
                }
            }
            encode_tx_value(output, new)?;
        }
        TxOp::RetractEntity(entity) => {
            output.push(3);
            encode_entity_ref(output, entity)?;
        }
        TxOp::Ensure { entity, spec } => {
            output.push(4);
            encode_entity_ref(output, entity)?;
            encode_entity_ref(output, spec)?;
        }
        TxOp::InstallAttribute(attribute) => {
            output.push(5);
            encode_attribute(output, attribute)?;
        }
        TxOp::AlterAttribute(attribute) => {
            output.push(6);
            encode_attribute(output, attribute)?;
        }
        TxOp::ForcePartition { tempid, partition } => {
            output.push(7);
            put_string(output, tempid)?;
            encode_entity_ref(output, partition)?;
        }
        TxOp::MatchPartition { tempid, entity } => {
            output.push(8);
            put_string(output, tempid)?;
            encode_entity_ref(output, entity)?;
        }
    }
    Ok(())
}

/// Canonical encoding for transaction forms that are safe to submit to the
/// authoritative transactor. Process-local callbacks deliberately have no
/// persistent/request representation.
fn encode_persistent_tx_form(output: &mut Vec<u8>, form: &TxForm) -> Result<(), SemanticError> {
    match form {
        TxForm::Edn(form) => {
            output.push(3);
            put_string(output, form.canonical_edn())?;
        }
        TxForm::Op(op) => {
            output.push(0);
            encode_tx_op(output, op)?;
        }
        TxForm::EntityMap(map) => {
            output.push(1);
            encode_entity_map(output, map, 0)?;
        }
        TxForm::ProgramCall(call) => {
            output.push(2);
            encode_program_call(output, call)?;
        }
        TxForm::Call(_) => {
            return Err(SemanticError::incorrect(
                "service/process-local-call",
                "process-local Rust transaction callbacks cannot cross the authoritative service boundary",
            ));
        }
    }
    Ok(())
}

pub(crate) fn persistent_tx_form_bytes(form: &TxForm) -> Result<usize, SemanticError> {
    let mut encoded = Vec::new();
    encode_persistent_tx_form(&mut encoded, form)?;
    Ok(encoded.len())
}

fn encode_entity_map(
    output: &mut Vec<u8>,
    map: &EntityMap,
    depth: usize,
) -> Result<(), SemanticError> {
    if depth > 32 {
        return Err(SemanticError::incorrect(
            "transaction/map-depth",
            "entity maps may contain at most 32 nested maps",
        ));
    }
    match &map.id {
        Some(entity) => {
            output.push(1);
            encode_entity_ref(output, entity)?;
        }
        None => output.push(0),
    }

    // Map entry order is not transaction semantics. Sorting the complete
    // encoded entry also gives deterministic ordering when malformed input
    // repeats an attribute; semantic validation remains the normalizer's job.
    let mut entries = map
        .attributes
        .iter()
        .map(|(attribute, value)| {
            let mut encoded = Vec::new();
            encode_attribute_ref(&mut encoded, attribute)?;
            encode_map_value(&mut encoded, value, depth)?;
            Ok(encoded)
        })
        .collect::<Result<Vec<_>, SemanticError>>()?;
    entries.sort();
    put_len(output, entries.len())?;
    for entry in entries {
        put_framed_bytes(output, &entry)?;
    }
    Ok(())
}

fn encode_attribute_ref(
    output: &mut Vec<u8>,
    attribute: &AttributeRef,
) -> Result<(), SemanticError> {
    match attribute {
        AttributeRef::Id(attribute) => {
            output.push(0);
            put_u32(output, *attribute);
        }
        AttributeRef::Ident(ident) => {
            output.push(1);
            encode_keyword(output, ident)?;
        }
        AttributeRef::ReverseId(attribute) => {
            output.push(2);
            put_u32(output, *attribute);
        }
        AttributeRef::ReverseIdent(ident) => {
            output.push(3);
            encode_keyword(output, ident)?;
        }
    }
    Ok(())
}

fn encode_map_value(
    output: &mut Vec<u8>,
    value: &MapValue,
    depth: usize,
) -> Result<(), SemanticError> {
    match value {
        MapValue::Value(value) => {
            output.push(0);
            encode_tx_value(output, value)?;
        }
        MapValue::Nested(map) => {
            output.push(1);
            encode_entity_map(output, map, depth + 1)?;
        }
        MapValue::Many(values) => {
            output.push(2);
            let mut values = values
                .iter()
                .map(|value| {
                    if matches!(value, MapValue::Many(_)) {
                        return Err(SemanticError::incorrect(
                            "transaction/nested-collection",
                            "map value collections cannot contain collections",
                        ));
                    }
                    let mut encoded = Vec::new();
                    encode_map_value(&mut encoded, value, depth)?;
                    Ok(encoded)
                })
                .collect::<Result<Vec<_>, SemanticError>>()?;
            values.sort();
            put_len(output, values.len())?;
            for value in values {
                put_framed_bytes(output, &value)?;
            }
        }
    }
    Ok(())
}

fn encode_program_call(output: &mut Vec<u8>, call: &ProgramCall) -> Result<(), SemanticError> {
    match &call.function {
        CallableRef::Database(entity) => {
            output.push(0);
            encode_entity_ref(output, entity)?;
        }
        CallableRef::ExactHash(hash) => {
            output.push(1);
            output.extend_from_slice(hash);
        }
        CallableRef::Local(symbol) => {
            output.push(2);
            encode_symbol(output, symbol)?;
        }
    }
    put_len(output, call.arguments.len())?;
    for argument in &call.arguments {
        encode_runtime_value(output, argument, 0)?;
    }
    Ok(())
}

fn encode_runtime_value(
    output: &mut Vec<u8>,
    value: &RuntimeValue,
    depth: usize,
) -> Result<(), SemanticError> {
    if depth > 16 {
        return Err(SemanticError::incorrect(
            "program/value-depth",
            "runtime values may contain at most 16 collection levels",
        ));
    }
    match value {
        RuntimeValue::Scalar(value) => {
            output.push(0);
            encode_value(output, value)?;
        }
        RuntimeValue::Entity(entity) => {
            output.push(1);
            encode_entity_ref(output, entity)?;
        }
        RuntimeValue::Null => output.push(2),
        RuntimeValue::Query(_) => {
            return Err(SemanticError::incorrect(
                "program/query-only-value",
                "general query data is not a portable transaction argument or anomaly",
            ));
        }
        RuntimeValue::Vector(values) => {
            output.push(3);
            put_len(output, values.len())?;
            for value in values {
                encode_runtime_value(output, value, depth + 1)?;
            }
        }
        RuntimeValue::Map(entries) => {
            if entries.windows(2).any(|entries| {
                entries[0].0.stored_cmp(&entries[1].0) != std::cmp::Ordering::Less
                    || entries[0].0.index_cmp(&entries[1].0) == std::cmp::Ordering::Equal
            }) {
                return Err(SemanticError::incorrect(
                    "program/noncanonical-map",
                    "runtime map keys must be unique and canonically ordered",
                ));
            }
            output.push(4);
            put_len(output, entries.len())?;
            for (key, value) in entries {
                encode_value(output, key)?;
                encode_runtime_value(output, value, depth + 1)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn program_call_digest(call: &ProgramCall) -> Result<Digest, SemanticError> {
    let mut bytes = Vec::new();
    encode_program_call(&mut bytes, call)?;
    Ok(sha256(&bytes))
}

fn encode_entity_ref(output: &mut Vec<u8>, entity: &EntityRef) -> Result<(), SemanticError> {
    crate::transaction::input::validate_entity_input(entity)?;
    match entity {
        EntityRef::Id(id) => {
            output.push(0);
            put_u64(output, *id);
        }
        EntityRef::Ident(ident) => {
            output.push(1);
            encode_keyword(output, ident)?;
        }
        EntityRef::Temp(tempid) => {
            output.push(2);
            put_string(output, tempid)?;
        }
        EntityRef::Lookup { attribute, value } => {
            output.push(3);
            put_u32(output, *attribute);
            encode_value(output, value)?;
        }
        EntityRef::Tx => output.push(4),
        EntityRef::Edn(text) => {
            output.push(6);
            put_string(output, &crate::edn_transaction::canonical_entity_ref(text)?)?;
        }
        EntityRef::LookupInput { attribute, value } => {
            output.push(5);
            put_u32(output, *attribute);
            encode_tx_value(output, value)?;
        }
    }
    Ok(())
}

fn decode_entity_ref(cursor: &mut Cursor<'_>) -> Result<EntityRef, SemanticError> {
    decode_entity_ref_at(cursor, 0)
}

fn input_depth(depth: usize) -> Result<(), SemanticError> {
    if depth > 32 {
        return Err(fault(
            "encoding/input-depth",
            "transaction reference input exceeds the 32-level admission policy",
        ));
    }
    Ok(())
}

fn decode_entity_ref_at(cursor: &mut Cursor<'_>, depth: usize) -> Result<EntityRef, SemanticError> {
    input_depth(depth)?;
    Ok(match cursor.u8()? {
        0 => EntityRef::Id(cursor.u64()?),
        1 => EntityRef::Ident(decode_keyword(cursor)?),
        2 => EntityRef::Temp(cursor.string()?),
        3 => EntityRef::Lookup {
            attribute: cursor.u32()?,
            value: decode_value(cursor, 0)?,
        },
        4 => EntityRef::Tx,
        6 => {
            let text = cursor.string()?;
            let canonical = crate::edn_transaction::canonical_entity_ref(&text)?;
            if canonical != text {
                return Err(fault(
                    "transport/noncanonical-edn",
                    "EDN entity reference is not canonical",
                ));
            }
            EntityRef::Edn(canonical)
        }
        5 => EntityRef::LookupInput {
            attribute: cursor.u32()?,
            value: Box::new(decode_tx_value_at(cursor, depth + 1, false)?),
        },
        tag => return Err(invalid_tag("entity reference", tag)),
    })
}

fn encode_tx_value(output: &mut Vec<u8>, value: &TxValue) -> Result<(), SemanticError> {
    crate::transaction::input::validate_value_input(value)?;
    match value {
        TxValue::Scalar(value) => {
            output.push(0);
            encode_value(output, value)
        }
        TxValue::Entity(entity) => {
            output.push(1);
            encode_entity_ref(output, entity)
        }
        TxValue::Tuple(slots) => {
            if !(2..=8).contains(&slots.len())
                || slots
                    .iter()
                    .flatten()
                    .any(|slot| matches!(slot, TxValue::Tuple(_)))
            {
                return Err(SemanticError::incorrect(
                    "encoding/invalid-input-tuple",
                    "input tuples require 2–8 non-tuple slots",
                ));
            }
            output.push(2);
            put_len(output, slots.len())?;
            for slot in slots {
                output.push(u8::from(slot.is_some()));
                if let Some(slot) = slot {
                    encode_tx_value(output, slot)?;
                }
            }
            Ok(())
        }
    }
}

fn decode_tx_value(cursor: &mut Cursor<'_>) -> Result<TxValue, SemanticError> {
    decode_tx_value_at(cursor, 0, false)
}

fn decode_tx_value_at(
    cursor: &mut Cursor<'_>,
    depth: usize,
    nested: bool,
) -> Result<TxValue, SemanticError> {
    input_depth(depth)?;
    Ok(match cursor.u8()? {
        0 => TxValue::Scalar(decode_value(cursor, 0)?),
        1 => TxValue::Entity(decode_entity_ref_at(cursor, depth + 1)?),
        2 => {
            let count = cursor.collection_len()?;
            if nested || !(2..=8).contains(&count) {
                return Err(fault(
                    "encoding/invalid-input-tuple",
                    "input tuples require 2–8 non-tuple slots",
                ));
            }
            let mut slots = Vec::with_capacity(count);
            for _ in 0..count {
                slots.push(if cursor.boolean()? {
                    Some(decode_tx_value_at(cursor, depth + 1, true)?)
                } else {
                    None
                });
            }
            TxValue::Tuple(slots)
        }
        tag => return Err(invalid_tag("transaction value", tag)),
    })
}

fn encode_keyword(output: &mut Vec<u8>, value: &Keyword) -> Result<(), SemanticError> {
    put_optional_string(output, value.namespace.as_deref())?;
    put_string(output, &value.name)
}

fn decode_keyword(cursor: &mut Cursor<'_>) -> Result<Keyword, SemanticError> {
    Ok(Keyword {
        namespace: cursor.optional_string()?,
        name: cursor.string()?,
    })
}

fn encode_symbol(output: &mut Vec<u8>, value: &Symbol) -> Result<(), SemanticError> {
    put_optional_string(output, value.namespace.as_deref())?;
    put_string(output, &value.name)
}

fn decode_symbol(cursor: &mut Cursor<'_>) -> Result<Symbol, SemanticError> {
    Ok(Symbol {
        namespace: cursor.optional_string()?,
        name: cursor.string()?,
    })
}

fn put_optional_string(output: &mut Vec<u8>, value: Option<&str>) -> Result<(), SemanticError> {
    match value {
        None => output.push(0),
        Some(value) => {
            output.push(1);
            put_string(output, value)?;
        }
    }
    Ok(())
}

fn put_bigint(output: &mut Vec<u8>, value: &BigInt) -> Result<(), SemanticError> {
    put_bytes(output, &value.to_signed_bytes_be())
}

fn put_string(output: &mut Vec<u8>, value: &str) -> Result<(), SemanticError> {
    put_bytes(output, value.as_bytes())
}

fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), SemanticError> {
    if bytes.len() > MAX_VALUE_LEN {
        return Err(SemanticError::incorrect(
            "encoding/value-too-large",
            format!("encoded value exceeds {MAX_VALUE_LEN} bytes"),
        ));
    }
    put_len(output, bytes.len())?;
    output.extend_from_slice(bytes);
    Ok(())
}

// A compound form contains scalar bytes plus framing/other values. Its limit
// is the enclosing blob's, not the smaller individual scalar-value ceiling.
// The framing bytes are unchanged, preserving existing request digests.
fn put_framed_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), SemanticError> {
    if bytes.len() > MAX_BLOB_LEN {
        return Err(SemanticError::incorrect(
            "encoding/blob-too-large",
            "compound form exceeds blob limit",
        ));
    }
    put_len(output, bytes.len())?;
    output.extend_from_slice(bytes);
    Ok(())
}

fn put_len(output: &mut Vec<u8>, length: usize) -> Result<(), SemanticError> {
    let length = u32::try_from(length).map_err(|_| {
        SemanticError::incorrect(
            "encoding/collection-too-large",
            "encoded collection length exceeds u32",
        )
    })?;
    put_u32(output, length);
    Ok(())
}

fn put_bool(output: &mut Vec<u8>, value: bool) {
    output.push(u8::from(value));
}

fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_i64(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn canonical_f32_bits(value: f32) -> u32 {
    if value.is_nan() {
        0x7fc0_0000
    } else if value == 0.0 {
        0
    } else {
        value.to_bits()
    }
}

fn canonical_f64_bits(value: f64) -> u64 {
    if value.is_nan() {
        0x7ff8_0000_0000_0000
    } else if value == 0.0 {
        0
    } else {
        value.to_bits()
    }
}

fn value_type_tag(value_type: ValueType) -> u8 {
    match value_type {
        ValueType::BigDec => 0,
        ValueType::BigInt => 1,
        ValueType::Boolean => 2,
        ValueType::Bytes => 3,
        ValueType::Double => 4,
        ValueType::Float => 5,
        ValueType::Instant => 6,
        ValueType::Keyword => 7,
        ValueType::Long => 8,
        ValueType::Ref => 9,
        ValueType::String => 10,
        ValueType::Symbol => 11,
        ValueType::Tuple => 12,
        ValueType::Uuid => 13,
        ValueType::Uri => 14,
        ValueType::Function => 15,
    }
}

fn invalid_tag(kind: &str, tag: u8) -> SemanticError {
    fault("encoding/invalid-tag", format!("invalid {kind} tag {tag}"))
}

fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn finish(&self) -> Result<(), SemanticError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(fault(
                "encoding/trailing-bytes",
                "durable payload contains trailing bytes",
            ))
        }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], SemanticError> {
        let end = self.position.checked_add(length).ok_or_else(|| {
            fault(
                "encoding/length-overflow",
                "durable payload length overflow",
            )
        })?;
        let value = self.bytes.get(self.position..end).ok_or_else(|| {
            fault(
                "encoding/truncated-payload",
                "durable payload ended before its declared value",
            )
        })?;
        self.position = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], SemanticError> {
        self.take(N)?
            .try_into()
            .map_err(|_| fault("encoding/truncated-payload", "invalid fixed-size value"))
    }

    fn u8(&mut self) -> Result<u8, SemanticError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, SemanticError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, SemanticError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, SemanticError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, SemanticError> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    fn digest(&mut self) -> Result<Digest, SemanticError> {
        self.array()
    }

    fn collection_len(&mut self) -> Result<usize, SemanticError> {
        let length = self.u32()? as usize;
        if length > MAX_COLLECTION_LEN {
            return Err(fault(
                "encoding/collection-too-large",
                format!("encoded collection exceeds {MAX_COLLECTION_LEN} entries"),
            ));
        }
        Ok(length)
    }

    fn bytes(&mut self) -> Result<&'a [u8], SemanticError> {
        // Byte/string payload length is bounded by MAX_VALUE_LEN, not by the
        // lower entry-count guard used for vectors and maps. Conflating the
        // two made legal multi-megabyte scalar values encode but fail their
        // mandatory canonical decode.
        let length = self.u32()? as usize;
        if length > MAX_VALUE_LEN {
            return Err(fault(
                "encoding/value-too-large",
                format!("encoded value exceeds {MAX_VALUE_LEN} bytes"),
            ));
        }
        self.take(length)
    }

    fn string(&mut self) -> Result<String, SemanticError> {
        let bytes = self.bytes()?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| fault("encoding/invalid-utf8", "encoded string is not valid UTF-8"))
    }

    fn optional_string(&mut self) -> Result<Option<String>, SemanticError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.string()?)),
            tag => Err(invalid_tag("optional string", tag)),
        }
    }

    fn boolean(&mut self) -> Result<bool, SemanticError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            tag => Err(invalid_tag("boolean", tag)),
        }
    }

    fn bigint(&mut self) -> Result<BigInt, SemanticError> {
        let bytes = self.bytes()?;
        let value = BigInt::from_signed_bytes_be(bytes);
        if value.to_signed_bytes_be() != bytes {
            return Err(fault(
                "encoding/noncanonical-bigint",
                "BigInteger contains redundant sign bytes",
            ));
        }
        Ok(value)
    }
}
