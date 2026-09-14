//! Time-local UUID values, not transaction clocks or allocation reservations.
//! Squuid follows recovered datomic/common.clj; UUIDv7 uses RFC9562 section5.7.
use crate::{ErrorCategory, SemanticError};
use rand::TryRng;
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION_MASK: u128 = 0xf << 76;
const VARIANT_MASK: u128 = 3 << 62;
const V7_MAX_MILLIS: u64 = (1 << 48) - 1;

fn unix_millis() -> Result<u64, SemanticError> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        SemanticError::incorrect("uuid/clock-before-epoch", "UUID clock is before Unix epoch")
    })?;
    u64::try_from(elapsed.as_millis()).map_err(|_| {
        SemanticError::incorrect(
            "uuid/clock-out-of-range",
            "UUID clock exceeds supported range",
        )
    })
}

fn random_bits() -> Result<u128, SemanticError> {
    // Stateless OS randomness avoids duplicating a process-local RNG stream
    // after fork. Failure is explicit rather than falling back to clock-only IDs.
    let mut bytes = [0u8; 16];
    rand::rngs::SysRng.try_fill_bytes(&mut bytes).map_err(|_| {
        SemanticError::new(
            ErrorCategory::Unavailable,
            "uuid/randomness-unavailable",
            "operating-system UUID randomness is unavailable",
        )
    })?;
    Ok(u128::from_be_bytes(bytes))
}

fn versioned(bits: u128, version: u128) -> u128 {
    (bits & !VERSION_MASK & !VARIANT_MASK) | (version << 76) | (2 << 62)
}

/// Semi-sequential UUID with Unix seconds in its leading32bits, version4 and
/// the standard variant. Remaining90bits are random. Reveals coarse creation
/// time; no strict ordering within a second or after clock regression.
pub fn squuid() -> Result<u128, SemanticError> {
    squuid_at(unix_millis()?)
}

/// Generate a squuid with a supplied Unix-millisecond clock. Precision is one
/// second; times whose seconds do not fit32bits are rejected, never wrapped.
pub fn squuid_at(unix_millis: u64) -> Result<u128, SemanticError> {
    let seconds = squuid_seconds(unix_millis)?;
    Ok(squuid_parts(seconds, random_bits()?))
}

fn squuid_seconds(millis: u64) -> Result<u32, SemanticError> {
    u32::try_from(millis / 1000).map_err(|_| {
        SemanticError::incorrect(
            "uuid/squuid-time-out-of-range",
            "squuid seconds exceed32bits",
        )
    })
}

fn squuid_parts(seconds: u32, random: u128) -> u128 {
    versioned((u128::from(seconds) << 96) | (random & ((1 << 96) - 1)), 4)
}

/// Read the leading32bits as Unix seconds, expressed in milliseconds. Like the
/// Datomic helper, this cannot establish that an arbitrary UUID was a squuid.
pub fn squuid_time_millis(uuid: u128) -> u64 {
    ((uuid >> 96) as u64) * 1000
}

/// RFC9562 UUIDv7:48-bit Unix milliseconds, version7/standard variant and74random
/// bits. No counter or monotonic-clock state; same-tick IDs have random order.
pub fn uuid_v7() -> Result<u128, SemanticError> {
    uuid_v7_at(unix_millis()?)
}

/// Generate UUIDv7 using an explicit Unix-millisecond clock. Useful for injected
/// application clocks. Retain generated intent when retrying a transaction;
/// calling this again produces a different UUID, not an idempotency key lookup.
pub fn uuid_v7_at(unix_millis: u64) -> Result<u128, SemanticError> {
    if unix_millis > V7_MAX_MILLIS {
        return Err(SemanticError::incorrect(
            "uuid/v7-time-out-of-range",
            "UUIDv7 milliseconds exceed48bits",
        ));
    }
    Ok(v7_parts(unix_millis, random_bits()?))
}

fn v7_parts(millis: u64, random: u128) -> u128 {
    versioned((u128::from(millis) << 80) | (random & ((1 << 80) - 1)), 7)
}

/// Extract the timestamp after checking the version and variant. A UUID is not
/// authenticated provenance; this field need not be the actual creation time.
pub fn uuid_v7_time_millis(uuid: u128) -> Result<u64, SemanticError> {
    if (uuid & VERSION_MASK) >> 76 != 7 || (uuid & VARIANT_MASK) >> 62 != 2 {
        return Err(SemanticError::incorrect(
            "uuid/not-v7",
            "UUID is not version7/standard variant",
        ));
    }
    Ok((uuid >> 80) as u64)
}
