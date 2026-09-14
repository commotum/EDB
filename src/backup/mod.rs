//! Differential current-format backup of Rust-owned immutable objects.
//! Portable repositories are file sinks/readers, not another live engine.
use crate::storage::root::{Block, DatabaseRoot, DatabaseValueRoot};
use crate::storage::{
    BlockDatabase, BlockReadConfig, BlockReader, IndexDescriptor, ObjectId, PgBlockStore,
    SnapshotMetadata,
};
use crate::storage::{ObjectReader, ObjectWriter};
use crate::{Database, Digest, MaintenanceControl, PostgresConnectionConfig, SemanticError};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

mod capture;
mod model;
mod point;
pub(crate) mod repository;
mod restore;
pub(crate) mod snapshot;
mod verify;
pub(crate) use model::ReadPoint;
pub use model::*;
pub(crate) use point::{load_publication, load_selected, open_read_point, read_object};
use point::{parse_point_name, point_header, same_information};
use repository::fault;
pub(crate) use verify::{verify_read_point, walk_repository};
const POINT_KIND: u16 = 60;
