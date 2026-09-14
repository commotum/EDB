//! Submission data, exact reports and admitted-result waiting; no scheduling policy.
use crate::{DatabaseValue, Digest, ErrorCategory, ProgramCall, SemanticError, TxForm, TxOp};
use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct TransactionRequest {
    pub request_key: String,
    /// One unordered collection of declarative transaction-data forms.
    /// Persisted function calls and entity maps are transaction data, not a
    /// second out-of-band request channel.
    pub forms: Vec<TxForm>,
    pub compare_basis_t: Option<u64>,
    pub tx_instant_override: Option<i64>,
}

impl TransactionRequest {
    pub fn new(request_key: impl Into<String>, operations: Vec<TxOp>) -> Self {
        Self {
            request_key: request_key.into(),
            forms: operations.into_iter().map(TxForm::Op).collect(),
            compare_basis_t: None,
            tx_instant_override: None,
        }
    }

    pub fn from_forms(request_key: impl Into<String>, forms: Vec<TxForm>) -> Self {
        Self {
            request_key: request_key.into(),
            forms,
            compare_basis_t: None,
            tx_instant_override: None,
        }
    }

    pub fn with_forms(mut self, forms: impl IntoIterator<Item = TxForm>) -> Self {
        self.forms.extend(forms);
        self
    }

    pub fn comparing_basis(mut self, basis_t: u64) -> Self {
        self.compare_basis_t = Some(basis_t);
        self
    }

    pub fn calling(mut self, call: ProgramCall) -> Self {
        self.forms.push(TxForm::ProgramCall(call));
        self
    }

    pub fn calling_all(mut self, calls: impl IntoIterator<Item = ProgramCall>) -> Self {
        self.forms
            .extend(calls.into_iter().map(TxForm::ProgramCall));
        self
    }

    pub fn with_tx_instant(mut self, instant: i64) -> Self {
        self.tx_instant_override = Some(instant);
        self
    }
}

#[derive(Clone, Debug)]
pub struct ServiceTransactionReport {
    /// The exact immutable value assessed by the transactor. Native values
    /// retain their authenticated root and recent tier without materializing
    /// the complete database in either the writer or this report.
    pub db_before: DatabaseValue,
    /// The exact immutable successor installed after durable publication.
    pub db_after: DatabaseValue,
    pub basis_t: u64,
    pub tx_hash: Digest,
    pub tx_data: Vec<crate::Datom>,
    pub tempids: BTreeMap<String, u64>,
    pub replayed: bool,
    /// Present only for a diagnostic submission or configured telemetry.
    /// Not encoded in durable receipts or reconstructed remote reports.
    pub diagnostics: Option<Arc<crate::TransactionDiagnostics>>,
}

/// Finite committed frontier captured by an asynchronous indexing request.
///
/// `scheduled` means this call added indexing demand. False means an existing
/// request/job already covers the target, or it is already published. Neither
/// value is a completion receipt: use `Connection::sync_index(target_t, timeout)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexRequest {
    pub target_t: u64,
    pub scheduled: bool,
}

#[derive(Debug)]
pub struct TransactionTicket {
    pub(super) request_key: String,
    pub(super) request_key_hash: Digest,
    pub(super) receiver: mpsc::Receiver<Result<ServiceTransactionReport, SemanticError>>,
}

impl TransactionTicket {
    pub fn wait(self, timeout: Duration) -> Result<ServiceTransactionReport, SemanticError> {
        match self.receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(unknown_outcome(
                self.request_key_hash,
                "timed out waiting for an admitted transaction",
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(unknown_outcome(
                self.request_key_hash,
                "transaction worker disconnected after admission",
            )),
        }
    }

    pub fn request_key(&self) -> &str {
        &self.request_key
    }
}

fn unknown_outcome(request_key_hash: Digest, message: &str) -> SemanticError {
    SemanticError::new(
        ErrorCategory::UnknownOutcome,
        "service/unknown-outcome",
        message,
    )
    .detail("request_key_hash", hex_digest(&request_key_hash))
}

fn hex_digest(digest: &Digest) -> String {
    use std::fmt::Write;

    digest
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to String cannot fail");
            hex
        })
}
