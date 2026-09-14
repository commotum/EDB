//! Immutable snapshot conveniences; these values do not own a live peer or writer.
use super::{PeerIndexCursor, transaction_hash};
use crate::index::cursor::MergeSource;
use crate::index::tree::{TreeRangeResult, TreeSeekResult};
use crate::storage::BlockSnapshot;
use crate::{
    DatabaseValue, Datom, Digest, Entity, EntityIdentifier, ErrorCategory, IndexOrder, IndexPrefix,
    Keyword, PullPattern, Query, QueryControl, QueryExtensions, QueryInput, QueryOutcome,
    QueryValue, Schema, SemanticError,
};

/// One immutable database value and its observed physical publication.
#[derive(Clone)]
pub struct PeerSnapshot {
    pub(super) native: BlockSnapshot,
    pub(super) publication_revision: u64,
}

impl PeerSnapshot {
    pub fn basis_t(&self) -> u64 {
        self.native.basis_t()
    }
    pub fn eidx_frontier(&self) -> u64 {
        self.native.eidx_frontier()
    }
    pub fn last_tx_instant(&self) -> Option<i64> {
        self.native.last_tx_instant()
    }
    pub fn schema(&self) -> &Schema {
        self.native.schema()
    }
    pub fn database_value(&self) -> DatabaseValue {
        self.native.database_value()
    }
    pub fn durable_base_t(&self) -> Option<u64> {
        Some(self.native.indexed_basis_t())
    }
    pub fn durable_base_revision(&self) -> Option<u64> {
        Some(self.publication_revision)
    }
    pub fn transaction_hash(&self) -> Digest {
        transaction_hash(&self.native)
    }
    pub fn entid(&self, ident: &Keyword) -> Option<u64> {
        self.native.entid(ident)
    }
    pub fn ident(&self, entity: u64) -> Option<&Keyword> {
        self.native.ident(entity)
    }
    pub fn entity(
        &self,
        identifier: impl Into<EntityIdentifier>,
    ) -> Result<Option<Entity>, SemanticError> {
        self.database_value().entity(identifier)
    }
    pub fn query(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QueryOutcome, SemanticError> {
        self.database_value().query(query, inputs, control)
    }
    pub fn query_with_extensions(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: &QueryExtensions,
    ) -> Result<QueryOutcome, SemanticError> {
        self.database_value()
            .query_with_extensions(query, inputs, control, extensions)
    }
    pub fn pull(
        &self,
        pattern: &PullPattern,
        entity: impl Into<EntityIdentifier>,
    ) -> Result<QueryValue, SemanticError> {
        self.database_value().pull(pattern, entity)
    }
    fn ensure_order(&self, order: IndexOrder) -> Result<(), SemanticError> {
        if order == IndexOrder::Avet && !self.native.avet_unready().is_empty() {
            return Err(SemanticError::new(
                ErrorCategory::Unavailable,
                "peer/avet-not-ready",
                "Captured AVET projections are incomplete",
            ));
        }
        Ok(())
    }
    pub fn range_cursor(
        &self,
        history: bool,
        order: IndexOrder,
        start: Option<&Datom>,
        end: Option<&Datom>,
    ) -> Result<PeerIndexCursor, SemanticError> {
        self.ensure_order(order)?;
        if let (Some(start), Some(end)) = (start, end)
            && !start.cmp_in(end, order).is_lt()
        {
            return Err(SemanticError::incorrect(
                "peer/invalid-tree-range",
                "Tree range start must precede its exclusive end",
            ));
        }
        Ok(PeerIndexCursor::block(
            self.native.range_cursor(history, order, start, end)?,
        ))
    }
    pub fn prefix_cursor(
        &self,
        history: bool,
        prefix: &IndexPrefix,
    ) -> Result<PeerIndexCursor, SemanticError> {
        Ok(PeerIndexCursor::block(
            self.native.prefix_cursor(history, prefix)?,
        ))
    }
    pub fn seek(
        &self,
        history: bool,
        order: IndexOrder,
        key: &Datom,
    ) -> Result<TreeSeekResult, SemanticError> {
        let mut cursor = self.range_cursor(history, order, Some(key), None)?;
        let datom = cursor.next().transpose()?;
        Ok(TreeSeekResult {
            datom,
            stats: cursor.stats().tree,
        })
    }
    pub fn range(
        &self,
        history: bool,
        order: IndexOrder,
        start: Option<&Datom>,
        end: Option<&Datom>,
    ) -> Result<TreeRangeResult, SemanticError> {
        collect(self.range_cursor(history, order, start, end)?)
    }
    pub fn datoms(
        &self,
        history: bool,
        order: IndexOrder,
    ) -> Result<TreeRangeResult, SemanticError> {
        self.range(history, order, None, None)
    }
    pub fn datoms_with_prefix(
        &self,
        history: bool,
        prefix: &IndexPrefix,
    ) -> Result<TreeRangeResult, SemanticError> {
        collect(self.prefix_cursor(history, prefix)?)
    }
}

fn collect(mut cursor: PeerIndexCursor) -> Result<TreeRangeResult, SemanticError> {
    let datoms = cursor.by_ref().collect::<Result<Vec<_>, _>>()?;
    Ok(TreeRangeResult {
        datoms,
        stats: cursor.stats().tree,
    })
}
impl DatabaseValue {
    /// Adopt an already captured peer snapshot as an immutable database value.
    pub fn native(snapshot: PeerSnapshot) -> Self {
        snapshot.database_value()
    }
}
impl From<PeerSnapshot> for DatabaseValue {
    fn from(snapshot: PeerSnapshot) -> Self {
        Self::native(snapshot)
    }
}
impl From<&PeerSnapshot> for DatabaseValue {
    fn from(snapshot: &PeerSnapshot) -> Self {
        Self::native(snapshot.clone())
    }
}
