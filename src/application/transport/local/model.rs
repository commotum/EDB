use super::*;

/// Explicit deployment policies, not limits on Datalog or database size.
#[derive(Clone, Copy, Debug)]
pub struct LocalTransportConfig {
    pub max_in_flight: usize,
    /// Total deadline for one request/response exchange, including clients
    /// that trickle bytes. Expiry after admission has an unknown outcome.
    pub request_timeout: Duration,
    pub max_frame_bytes: usize,
}

impl Default for LocalTransportConfig {
    fn default() -> Self {
        Self {
            max_in_flight: 4,
            request_timeout: Duration::from_secs(30),
            max_frame_bytes: MAX_FRAME,
        }
    }
}

impl LocalTransportConfig {
    /// Validate deployment bounds without starting a writer or binding a socket.
    pub fn validate(&self) -> Result<(), SemanticError> {
        if self.max_in_flight == 0
            || self.max_in_flight > 256
            || self.request_timeout.is_zero()
            || self.max_frame_bytes < 48
            || self.max_frame_bytes > MAX_FRAME
            || Instant::now().checked_add(self.request_timeout).is_none()
        {
            return Err(SemanticError::incorrect(
                "transport/config",
                "invalid bounded transport settings",
            ));
        }
        Ok(())
    }
}

/// A confirmed durable success, even when opening its local native values
/// fails. Do not resubmit on `report: Err` as if the transaction were rejected;
/// use the stable request key to retrieve the same receipt after recovery.
#[derive(Debug)]
pub struct CommittedTransaction {
    pub basis_t: u64,
    pub tx_hash: Digest,
    pub replayed: bool,
    pub report: Result<ServiceTransactionReport, SemanticError>,
}
