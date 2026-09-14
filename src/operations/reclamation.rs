//! Retired-identity admission to the shared incremental collector. A completed
//! cycle is not a promise to erase objects still owned by held values/receipts.
use super::PostgresOperator;
use crate::SemanticError;
use crate::storage::ownership::{CollectionPhase, CollectionStats};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct RetiredDatabaseReclamation {
    pub storage_id: String,
    pub lineage_id: String,
    pub phase: Option<CollectionPhase>,
    pub collection: CollectionStats,
    pub cycle_complete: bool,
    pub applied: bool,
}
impl PostgresOperator {
    pub fn preview_retired_database_reclamation(
        &mut self,
        id: &str,
        lineage: &str,
        age: Duration,
    ) -> Result<RetiredDatabaseReclamation, SemanticError> {
        self.database_reclamation(id, lineage, age, false)
    }
    pub fn reclaim_retired_database(
        &mut self,
        id: &str,
        lineage: &str,
        age: Duration,
    ) -> Result<RetiredDatabaseReclamation, SemanticError> {
        self.database_reclamation(id, lineage, age, true)
    }
    fn database_reclamation(
        &mut self,
        id: &str,
        lineage: &str,
        age: Duration,
        apply: bool,
    ) -> Result<RetiredDatabaseReclamation, SemanticError> {
        let mut store = crate::storage::PgBlockStore::connect(&self.connection)?;
        let entry = crate::storage::catalog::route_entry(
            &mut store,
            crate::storage::catalog::parse_identity(id)?,
        )?;
        if entry.lineage_id != lineage {
            return Err(SemanticError::conflict(
                "catalog/identity-mismatch",
                "Reclamation lineage differs from its issued identity",
            ));
        }
        if !entry.retired {
            return Err(SemanticError::incorrect(
                "operations/database-not-retired",
                "Retire the database route before reclamation",
            ));
        }
        let inventory = if apply {
            self.collect_garbage(age)?
        } else {
            self.garbage_inventory(age)?
        };
        Ok(RetiredDatabaseReclamation {
            storage_id: id.into(),
            lineage_id: lineage.into(),
            phase: inventory.phase,
            cycle_complete: inventory.phase == Some(CollectionPhase::Complete),
            collection: inventory.collection,
            applied: apply,
        })
    }
}
