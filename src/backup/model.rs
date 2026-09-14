use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupPoint {
    pub lineage_id: String,
    pub log_generation: u64,
    pub basis_t: u64,
    pub manifest_hash: Digest,
    pub objects_written: usize,
    pub objects_reused: usize,
}
#[derive(Clone, Debug)]
pub struct BackupVerification {
    pub point: BackupPoint,
    pub database: Database,
    pub objects_read: usize,
}
/// The durable result of activating an immutable backup point.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreResult {
    pub point: BackupPoint,
    pub target: String,
    pub activated_root: Digest,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[doc(hidden)]
pub enum BackupFault {
    #[default]
    None,
    AfterPublicationCaptured,
    AfterFirstObjectStaged,
    AfterObjects,
    AfterManifestStaged,
    AfterManifestPublished,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[doc(hidden)]
pub enum RestoreFault {
    #[default]
    None,
    AfterFirstContentInserted,
    BeforeCommit,
    AfterCommitBeforeResponse,
    AfterTreePublication,
}
pub struct PortableBackup {
    pub(crate) connection: PostgresConnectionConfig,
    pub(crate) maintenance: MaintenanceControl,
}
#[derive(Clone, Debug)]
pub(crate) struct ReadPoint {
    pub point: BackupPoint,
    pub publication: ObjectId,
    pub value: DatabaseValueRoot,
    pub open_object_reads: u64,
    pub open_object_bytes: u64,
}
