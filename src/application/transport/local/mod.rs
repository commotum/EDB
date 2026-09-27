//! Same-host independent-process transaction delivery. The server uses a
//! private directory and a mode-0600 Unix socket. There is no TCP
//! listener, implicit trust of a shared writable pathname, or durable mailbox
//! retaining transaction inputs. PostgreSQL remains the sole durable authority.
use crate::encoding::{
    WireOutcome, decode_submission, decode_submission_outcome, encode_submission,
    encode_submission_outcome,
};
use crate::{
    Connection, Digest, ErrorCategory, SemanticError, ServiceTransactionReport, TransactionClient,
    TransactionRequest,
};
use std::fs::{File, Metadata, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_FRAME: usize = 64 * 1024 * 1024 + 48;
const ENDPOINT_LOCK_HEADER: &str = "EDB-LOCAL-ENDPOINT 1\n";

mod client;
mod endpoint;
mod model;
mod server;
mod wire;
use endpoint::EndpointOwner;
pub use endpoint::LocalTransactionEndpoint;
pub use model::{CommittedTransaction, LocalTransportConfig};
pub use server::LocalTransactionServer;
use wire::*;
