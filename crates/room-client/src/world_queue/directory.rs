use super::{Activation, MAX_DIRECTORY, RecoveryReason, WorldOutbox, active, invalid, quarantine, read_u64, sql};
use crate::Result;
use photocraft_scene::CanvasId;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};

impl WorldOutbox {
    /// Candidates only: a partial directory never changes an existing grant.
    pub(crate) fn directory_grants(&self, lease: &Activation) -> Result<BTreeMap<CanvasId, u64>> {
        active(&self.db, lease)?;
        let mut query = self.db.prepare("SELECT canvas,epoch FROM grants WHERE scope=?1 AND blocked IN(0,2) ORDER BY canvas LIMIT ?2").map_err(sql)?;
        let mut rows = query.query(params![lease.key, (MAX_DIRECTORY + 1) as i64]).map_err(sql)?;
        let mut grants = BTreeMap::new();
        while let Some(row) = rows.next().map_err(sql)? {
            if grants.len() == MAX_DIRECTORY {
                return Err(invalid("Saved canvas directory exceeds its reconciliation limit; retain recovery data"));
            }
            let canvas = CanvasId::new(&row.get::<_, String>(0).map_err(sql)?).map_err(|_| invalid("Saved canvas identity"))?;
            let epoch = read_u64(row, 1).map_err(sql)?;
            if epoch == 0 {
                return Err(invalid("Saved canvas grant epoch"));
            }
            grants.insert(canvas, epoch);
        }
        Ok(grants)
    }

    /// Called only after the authenticated coordinator validates every page from
    /// the first cursor through the terminal page. Never deletes original art.
    pub(crate) fn reconcile_directory(
        &mut self,
        lease: &Activation,
        baseline: &BTreeMap<CanvasId, u64>,
        present: &BTreeSet<CanvasId>,
    ) -> Result<BTreeSet<CanvasId>> {
        if baseline.len() > MAX_DIRECTORY || present.len() > MAX_DIRECTORY {
            return Err(invalid("Canvas directory reconciliation exceeds its limit"));
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        active(&tx, lease)?;
        let mut removed = BTreeSet::new();
        for (canvas, epoch) in baseline {
            if present.contains(canvas) {
                continue;
            }
            let current: Option<u64> = tx
                .query_row("SELECT epoch FROM grants WHERE scope=?1 AND canvas=?2 AND blocked IN(0,2)", params![lease.key, canvas.as_str()], |row| {
                    read_u64(row, 0)
                })
                .optional()
                .map_err(sql)?;
            // An independently refreshed/new invitation must not be undone by
            // an older listing. New canvas grants are absent from the baseline.
            if current == Some(*epoch) {
                quarantine(&tx, lease, *canvas, RecoveryReason::AccessRemoved)?;
                removed.insert(*canvas);
            }
        }
        tx.commit().map_err(sql)?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_queue::WorldScope;
    use photocraft_scene::{
        OwnerId,
        shared::{DeviceId, Edit, WorldState},
    };

    #[test]
    fn directory_revocation_is_atomic_and_retains_original_rows_when_sqlite_rejects_commit() {
        let root = tempfile::tempdir().unwrap();
        let mut queue = WorldOutbox::open(root.path().join("directory.db")).unwrap();
        let scope =
            WorldScope::new("https://example.invalid", OwnerId::new(&format!("{:032x}", 1)).unwrap(), DeviceId::new(&format!("{:032x}", 2)).unwrap()).unwrap();
        let lease = queue.activate(scope).unwrap();
        let mut originals = Vec::new();
        for number in 1..=2 {
            let canvas = CanvasId::new(&format!("{number:032x}")).unwrap();
            queue.grant(&lease, canvas, 1, 1).unwrap();
            queue.save_snapshot(&lease, &WorldState::new(canvas, lease.scope.profile, "Saved".into()).unwrap()).unwrap();
            originals.push(queue.enqueue(&lease, canvas, Edit::Rename { name: "Unsent".into(), expected_revision: 0 }).unwrap());
        }
        let baseline = queue.directory_grants(&lease).unwrap();
        queue.db.execute_batch("CREATE TRIGGER reject_directory BEFORE UPDATE ON grants WHEN NEW.canvas='00000000000000000000000000000002' AND NEW.blocked=1 BEGIN SELECT RAISE(ABORT,'synthetic directory failure'); END;").unwrap();
        assert!(queue.reconcile_directory(&lease, &baseline, &BTreeSet::new()).is_err());
        assert_eq!(queue.cached_summaries(&lease, None, 128).unwrap().items.len(), 2);
        assert!(queue.recovery(&lease, 10).unwrap().is_empty());
        assert_eq!(queue.pending(&lease, 10).unwrap().into_iter().map(|queued| queued.operation).collect::<Vec<_>>(), originals);
        queue.db.execute_batch("DROP TRIGGER reject_directory;").unwrap();
        assert_eq!(queue.reconcile_directory(&lease, &baseline, &BTreeSet::new()).unwrap().len(), 2);
        assert!(queue.cached_summaries(&lease, None, 128).unwrap().items.is_empty());
        assert_eq!(queue.recovery(&lease, 10).unwrap().len(), 2);
        let rows: u64 = queue.db.query_row("SELECT count(*) FROM snapshots", [], |row| read_u64(row, 0)).unwrap();
        assert_eq!(rows, 2);
    }
}
