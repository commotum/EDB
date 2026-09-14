//! Untrusted, empty PostgreSQL wakeups. Only authenticated log/index reads
//! advance a peer; disconnects and lost hints are repaired by anti-entropy.
use crate::postgres_connection::postgres_error;
use crate::storage::PgBlockStore;
use crate::{PostgresConnectionConfig, SemanticError};
use std::future::{Future, poll_fn};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, mpsc};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_postgres::AsyncMessage;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NoticeListenerStats {
    pub received: u64,
    pub coalesced: u64,
    pub peak_batch: u64,
}
static RECEIVED: AtomicU64 = AtomicU64::new(0);
static LISTENER_COALESCED: AtomicU64 = AtomicU64::new(0);
static PEAK_BATCH: AtomicU64 = AtomicU64::new(0);
pub fn notice_listener_stats() -> NoticeListenerStats {
    NoticeListenerStats {
        received: RECEIVED.load(Ordering::Relaxed),
        coalesced: LISTENER_COALESCED.load(Ordering::Relaxed),
        peak_batch: PEAK_BATCH.load(Ordering::Relaxed),
    }
}
fn record_batch(count: u64) {
    RECEIVED.fetch_add(count, Ordering::Relaxed);
    LISTENER_COALESCED.fetch_add(count.saturating_sub(1), Ordering::Relaxed);
    PEAK_BATCH.fetch_max(count, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NoticePublisherStats {
    pub queued: u64,
    pub dropped: u64,
    pub coalesced: u64,
    pub sent: u64,
    pub failures: u64,
}
static QUEUED: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static COALESCED: AtomicU64 = AtomicU64::new(0);
static SENT: AtomicU64 = AtomicU64::new(0);
static FAILURES: AtomicU64 = AtomicU64::new(0);
pub fn notice_publisher_stats() -> NoticePublisherStats {
    NoticePublisherStats {
        queued: QUEUED.load(Ordering::Relaxed),
        dropped: DROPPED.load(Ordering::Relaxed),
        coalesced: COALESCED.load(Ordering::Relaxed),
        sent: SENT.load(Ordering::Relaxed),
        failures: FAILURES.load(Ordering::Relaxed),
    }
}
type Notice = (PostgresConnectionConfig, String);
static PUBLISHER: OnceLock<Option<mpsc::SyncSender<Notice>>> = OnceLock::new();

/// Never wait on network I/O or turn hint failure into a commit outcome.
/// One process-global worker retains at most 64 queued + 64 draining hints.
pub(crate) fn publish(config: &PostgresConnectionConfig, database: &str) {
    if database.len() > 4096 {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let sender = PUBLISHER.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<Notice>(64);
        std::thread::Builder::new()
            .name("atomic-notice-publisher".into())
            .spawn(move || {
                let context = crate::OperationContext::new(crate::OperationKind::PeerObservation);
                let _scope = context.enter();
                let mut current: Option<(PostgresConnectionConfig, PgBlockStore)> = None;
                while let Ok(first) = receiver.recv() {
                    let mut batch = vec![first];
                    for _ in 1..64 {
                        let Ok(next) = receiver.try_recv() else {
                            break;
                        };
                        if batch.contains(&next) {
                            COALESCED.fetch_add(1, Ordering::Relaxed);
                        } else {
                            batch.push(next);
                        }
                    }
                    for (config, database) in batch {
                        if current.as_ref().is_none_or(|(active, _)| active != &config) {
                            current = match PgBlockStore::connect(&config) {
                                Ok(client) => Some((config.clone(), client)),
                                Err(_) => {
                                    FAILURES.fetch_add(1, Ordering::Relaxed);
                                    continue;
                                }
                            };
                        }
                        let client = &mut current.as_mut().unwrap().1;
                        let result = client.notify_reference(&format!("databases/{database}"));
                        if result.is_ok() {
                            SENT.fetch_add(1, Ordering::Relaxed);
                        } else {
                            FAILURES.fetch_add(1, Ordering::Relaxed);
                            current = None;
                        }
                    }
                }
            })
            .ok()
            .map(|_| sender)
    });
    if let Some(sender) = sender
        && sender
            .try_send((config.clone(), database.to_owned()))
            .is_ok()
    {
        QUEUED.fetch_add(1, Ordering::Relaxed);
    } else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservationConfig {
    pub anti_entropy_interval: Duration,
}
impl Default for ObservationConfig {
    fn default() -> Self {
        Self {
            anti_entropy_interval: Duration::from_secs(30),
        }
    }
}
impl ObservationConfig {
    pub(crate) fn validate(self) -> Result<Self, SemanticError> {
        if self.anti_entropy_interval.is_zero()
            || std::time::Instant::now()
                .checked_add(self.anti_entropy_interval)
                .is_none()
        {
            return Err(SemanticError::incorrect(
                "observation/invalid-interval",
                "anti-entropy interval must be positive and representable",
            ));
        }
        Ok(self)
    }
}

trait NoticeConnection: Send {
    fn poll(&mut self, cx: &mut Context<'_>)
    -> Poll<Option<Result<AsyncMessage, postgres::Error>>>;
}
impl<S, T> NoticeConnection for tokio_postgres::Connection<S, T>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<AsyncMessage, postgres::Error>>> {
        self.poll_message(cx)
    }
}
pub(crate) struct NoticeListener {
    runtime: tokio::runtime::Runtime,
    _client: tokio_postgres::Client,
    connection: Box<dyn NoticeConnection>,
}
fn disconnected() -> SemanticError {
    SemanticError::new(
        crate::ErrorCategory::Unavailable,
        "observation/disconnected",
        "database notice connection disconnected",
    )
}
impl NoticeListener {
    pub(crate) fn connect(
        config: &PostgresConnectionConfig,
        database: &str,
    ) -> Result<Self, SemanticError> {
        let client = PgBlockStore::connect(config)?;
        let channel = client.notification_channel(&format!("databases/{database}"))?;
        // Provider-generated prefix + lowercase hex only, never caller SQL.
        if !channel.starts_with("atomic_r_")
            || channel.len() != 57
            || !channel[9..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(SemanticError::incorrect(
                "observation/invalid-channel",
                "database notice channel is invalid",
            ));
        }
        // No synchronous postgres Notifications iterator: that adapter
        // accumulates an unbounded VecDeque before yielding to its caller.
        drop(client);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| {
                SemanticError::new(
                    crate::ErrorCategory::Unavailable,
                    "observation/runtime",
                    "notification runtime could not start",
                )
            })?;
        let (prepared, tls) = config.listener_connection_parts("observation/connect", None)?;
        let (client, mut connection): (tokio_postgres::Client, Box<dyn NoticeConnection>) =
            crate::sql_io::observe_driver_call(crate::SqlCallKind::Connect, || {
                if let Some(tls) = tls {
                    runtime
                        .block_on(prepared.connect(tls))
                        .map(|(client, connection)| {
                            (client, Box::new(connection) as Box<dyn NoticeConnection>)
                        })
                        .map_err(|_| {
                            PostgresConnectionConfig::tls_connection_error("observation/connect")
                        })
                } else {
                    runtime
                        .block_on(prepared.connect(tokio_postgres::NoTls))
                        .map(|(client, connection)| {
                            (client, Box::new(connection) as Box<dyn NoticeConnection>)
                        })
                        .map_err(|e| postgres_error("observation/connect", e))
                }
            })?;
        let listen = format!("LISTEN {channel}");
        crate::sql_io::observe_driver_call(crate::SqlCallKind::BatchExecute, || {
            let request = client.batch_execute(&listen);
            let mut request = std::pin::pin!(request);
            runtime.block_on(poll_fn(|cx| {
                if let Poll::Ready(result) = request.as_mut().poll(cx) {
                    return Poll::Ready(
                        result.map_err(|e| postgres_error("observation/listen", e)),
                    );
                }
                let mut notices = 0;
                for _ in 0..64 {
                    match connection.poll(cx) {
                        Poll::Ready(Some(Ok(AsyncMessage::Notification(_)))) => notices += 1,
                        Poll::Ready(Some(Ok(_))) => {}
                        Poll::Ready(Some(Err(error))) => {
                            return Poll::Ready(Err(postgres_error("observation/listen", error)));
                        }
                        Poll::Ready(None) => return Poll::Ready(Err(disconnected())),
                        Poll::Pending => {
                            record_batch(notices);
                            return request
                                .as_mut()
                                .poll(cx)
                                .map(|r| r.map_err(|e| postgres_error("observation/listen", e)));
                        }
                    }
                }
                record_batch(notices);
                cx.waker().wake_by_ref();
                Poll::Pending
            }))
        })?;
        Ok(Self {
            runtime,
            _client: client,
            connection,
        })
    }
    pub(crate) fn wait(&mut self, timeout: Duration) -> Result<bool, SemanticError> {
        let connection = &mut self.connection;
        self.runtime.block_on(async {
            match tokio::time::timeout(
                timeout,
                poll_fn(|cx| {
                    let mut count = 0;
                    let mut exhausted = true;
                    for _ in 0..64 {
                        match connection.poll(cx) {
                            Poll::Ready(Some(Ok(AsyncMessage::Notification(_)))) => count += 1,
                            Poll::Ready(Some(Ok(_))) => {}
                            Poll::Ready(Some(Err(error))) => {
                                return Poll::Ready(Err(postgres_error(
                                    "observation/receive",
                                    error,
                                )));
                            }
                            Poll::Ready(None) => return Poll::Ready(Err(disconnected())),
                            Poll::Pending => {
                                exhausted = false;
                                break;
                            }
                        }
                    }
                    record_batch(count);
                    if count > 0 {
                        Poll::Ready(Ok(true))
                    } else {
                        if exhausted {
                            cx.waker().wake_by_ref();
                        }
                        Poll::Pending
                    }
                }),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Ok(false),
            }
        })
    }
}
