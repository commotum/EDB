//! Checked entity, partition and transaction coordinates. Allocation policy and
//! writer state belong to assessment, not these pure representation operations.

use crate::{ErrorCategory, SemanticError};

/// Number of low bits reserved for the entity index in EDB's entity ID representation.
pub const EIDX_BITS: u32 = 42;
pub const PARTITION_BITS: u32 = 20;
pub const EIDX_MASK: u64 = (1_u64 << EIDX_BITS) - 1;
/// Largest non-negative recovered `eid->eidx`. Bit 41 is the sign bit and is
/// reserved by the recovered representation for negative/tempid components.
pub const MAX_EIDX: u64 = (1_u64 << (EIDX_BITS - 1)) - 1;
pub const MAX_PARTITION: u32 = (1_u32 << PARTITION_BITS) - 1;
pub const MAX_EID: u64 = ((MAX_PARTITION as u64) << EIDX_BITS) | MAX_EIDX;

/// The three recovered named partitions retained by the native kernel.
pub const DB_PARTITION: u32 = 0;
pub const TX_PARTITION: u32 = 3;
pub const USER_PARTITION: u32 = 4;
/// The high half of the partition-bit space names implicit partitions.
pub const IMPLICIT_PARTITION_BASE: u32 = 1 << (PARTITION_BITS - 1);

/// Return one of 524288 implicit partition entity IDs without an installation
/// transaction. This is an entity ID, not the raw high-bit partition number.
pub fn implicit_part(number: u32) -> Result<u64, SemanticError> {
    if number >= IMPLICIT_PARTITION_BASE {
        return Err(SemanticError::incorrect(
            "identity/implicit-part-out-of-range",
            "implicit partition number must be below 524288",
        ));
    }
    make_eid(IMPLICIT_PARTITION_BASE | number, 0)
}

/// Invert `implicit_part`; ordinary entities and named partitions return None.
pub fn implicit_part_id(partition: u64) -> Result<Option<u32>, SemanticError> {
    let bits = eid_to_part(partition)?;
    Ok(
        (bits >= IMPLICIT_PARTITION_BASE && eid_to_eidx(partition)? == 0)
            .then_some(bits ^ IMPLICIT_PARTITION_BASE),
    )
}

/// Return the partition's entity ID, distinct from its raw `eid_to_part` bits.
pub fn partition_eid(entity: u64) -> Result<u64, SemanticError> {
    let bits = eid_to_part(entity)?;
    if bits < IMPLICIT_PARTITION_BASE {
        Ok(u64::from(bits))
    } else {
        make_eid(bits, 0)
    }
}

/// Initial exclusive issued-index frontier. This preserves the documented
/// ability to use previously issued small IDs while preventing callers from
/// minting IDs at or beyond the frontier.
pub const INITIAL_EIDX_FRONTIER: u64 = 1_000;

/// Construct a permanent entity ID without truncating either component.
pub fn make_eid(partition: u32, eidx: u64) -> Result<u64, SemanticError> {
    if partition > MAX_PARTITION {
        return Err(SemanticError::incorrect(
            "identity/partition-out-of-range",
            format!("partition {partition} exceeds {MAX_PARTITION}"),
        ));
    }
    if eidx > MAX_EIDX {
        return Err(SemanticError::incorrect(
            "identity/eidx-out-of-range",
            format!("entity index {eidx} exceeds {MAX_EIDX}"),
        ));
    }
    Ok((u64::from(partition) << EIDX_BITS) | eidx)
}

/// Recover the partition component of a permanent entity ID.
pub fn eid_to_part(eid: u64) -> Result<u32, SemanticError> {
    validate_permanent_eid(eid)?;
    Ok((eid >> EIDX_BITS) as u32)
}

/// Recover the non-negative entity-index component of a permanent entity ID.
pub fn eid_to_eidx(eid: u64) -> Result<u64, SemanticError> {
    validate_permanent_eid(eid)?;
    Ok(eid & EIDX_MASK)
}

/// Convert logical transaction time to its reified transaction entity ID.
pub fn t_to_tx(t: u64) -> Result<u64, SemanticError> {
    make_eid(TX_PARTITION, t)
}

/// Convert a reified transaction entity ID back to logical transaction time.
pub fn tx_to_t(tx: u64) -> Result<u64, SemanticError> {
    let partition = eid_to_part(tx)?;
    if partition != TX_PARTITION {
        return Err(SemanticError::incorrect(
            "identity/not-a-transaction-id",
            format!("entity id {tx} belongs to partition {partition}, not transaction partition 3"),
        ));
    }
    eid_to_eidx(tx)
}

pub(crate) fn validate_permanent_eid(eid: u64) -> Result<(), SemanticError> {
    if eid >> (EIDX_BITS + PARTITION_BITS) != 0 {
        return Err(SemanticError::new(
            ErrorCategory::Incorrect,
            "identity/eid-out-of-range",
            format!("entity id {eid} is not a permanent 62-bit entity id"),
        ));
    }
    if eid & EIDX_MASK > MAX_EIDX {
        return Err(SemanticError::new(
            ErrorCategory::Incorrect,
            "identity/negative-eidx",
            format!("entity id {eid} has a negative recovered entity-index component"),
        ));
    }
    Ok(())
}

pub(crate) fn validate_supported_eid(eid: u64) -> Result<(), SemanticError> {
    // Representation checks cannot consult a database. Allocation/transaction
    // admission additionally check named partition installation at db-before.
    validate_permanent_eid(eid)
}

pub(crate) fn validate_frontier(frontier: u64) -> Result<(), SemanticError> {
    if !(INITIAL_EIDX_FRONTIER..=MAX_EIDX + 1).contains(&frontier) {
        return Err(SemanticError::new(
            ErrorCategory::Fault,
            "identity/invalid-issued-frontier",
            format!(
                "issued entity-index frontier {frontier} is outside {INITIAL_EIDX_FRONTIER}..={}",
                MAX_EIDX + 1
            ),
        ));
    }
    Ok(())
}

/// Stable identity of one logical database catalog entry.
///
/// A public database name is a reusable address chosen by an operator. The
/// storage ID and lineage captured here never change on rename and never
/// retarget when a name is retired or reused for a new database.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DatabaseIdentity {
    database_id: String,
    lineage_id: String,
}

impl DatabaseIdentity {
    pub(crate) fn new(database_id: impl Into<String>, lineage_id: impl Into<String>) -> Self {
        Self {
            database_id: database_id.into(),
            lineage_id: lineage_id.into(),
        }
    }

    pub fn database_id(&self) -> &str {
        &self.database_id
    }

    pub fn lineage_id(&self) -> &str {
        &self.lineage_id
    }
}
