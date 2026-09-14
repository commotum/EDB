//! TLS-authenticated, lease-bound remote submission. PostgreSQL remains the
//! only durable authority; endpoint discovery contains no credentials.
use crate::encoding::{
    WireOutcome, decode_submission, decode_submission_outcome, encode_submission,
    encode_submission_outcome,
};
use crate::storage::{BatchOutcome, PgBlockStore, RefChange, RefCondition};
use crate::transactor::client::TransactorLease;
use crate::{
    CommittedTransaction, Connection, DatabaseIdentity, Digest, ErrorCategory,
    PostgresConnectionConfig, SemanticError, TransactionClient, TransactionHints,
    TransactionRequest,
};
use native_tls::{Certificate, Identity, Protocol, TlsAcceptor, TlsConnector};
use rand::TryRng;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
const PROTOCOL: u32 = 1;
const HELLO: &[u8; 8] = b"ATREMOTE";
const MAX_FRAME: usize = 64 * 1024 * 1024 + 48;
const MAX_HELLO: usize = 256 * 1024;

mod client;
pub(crate) mod config;
mod hints;
mod model;
pub(crate) mod routing;
mod server;
mod wire;
pub use model::{RemoteAuthToken, RemoteClientConfig, RemoteTransportConfig, RemoteWriterEndpoint};
pub use server::{RemoteTransactionEndpoint, RemoteTransactionServer, RemoteTransportStats};
use wire::*;
