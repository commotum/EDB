/// A documented Datomic time-point form.
///
/// `T` is the logical transaction coordinate, `Tx` is its reified transaction
/// entity, and `Instant` is Unix epoch milliseconds.  Instants are deliberately
/// explicit because they are less precise than T/Tx when transactions share a
/// millisecond.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimePoint {
    T(u64),
    Tx(u64),
    Instant(i64),
}

impl TimePoint {
    pub const fn t(t: u64) -> Self {
        Self::T(t)
    }

    pub const fn tx(tx: u64) -> Self {
        Self::Tx(tx)
    }

    pub const fn instant_millis(instant: i64) -> Self {
        Self::Instant(instant)
    }
}
