//! Advisory block reads use an independent driver and the existing bounded RAM
//! cache. Captured immutable state is independent of live peer observation.
use crate::{DatabaseValue, SemanticError};
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct HintReadPlan {
    snapshot: crate::storage::BlockSnapshot,
}

impl DatabaseValue {
    /// No I/O. Block snapshot resources have executor-owned final cleanup, so
    /// a detached hint cannot synchronously close the writer's driver on Drop.
    pub(crate) fn hint_prefetch_plan(&self) -> Result<HintReadPlan, SemanticError> {
        let (snapshot, ..) = self.committed_block_parts()?;
        Ok(HintReadPlan { snapshot })
    }
}

impl HintReadPlan {
    pub(crate) fn open(self, timeout: Duration) -> Result<DatabaseValue, SemanticError> {
        Ok(self
            .snapshot
            .fork_for_hints(timeout.min(Duration::from_secs(30)))?
            .database_value())
    }
}
