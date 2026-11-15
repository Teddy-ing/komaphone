use super::{Action, Coordinator, Event, Result, other, wire};
use crate::world_queue::MAX_DIRECTORY;
use photocraft_scene::CanvasId;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Scan {
    baseline: BTreeMap<CanvasId, u64>,
    present: BTreeSet<CanvasId>,
    next: Option<CanvasId>,
    request: Option<u64>,
}
impl Coordinator {
    pub(super) fn directory_request(&mut self, sequence: u64, action: &Action) {
        match action {
            Action::List { after: None, .. } => {
                self.directory_start = Some(sequence);
                self.directory_scan = None;
            }
            Action::List { after: Some(after), .. } => {
                if let Some(scan) = &mut self.directory_scan {
                    if scan.next == Some(*after) && scan.request.is_none() {
                        scan.request = Some(sequence);
                    } else {
                        // Interleaved or repeated pagination is not proof of a
                        // complete membership inventory. Keep the saved grants.
                        self.directory_scan = None;
                    }
                }
            }
            _ => {}
        }
    }
    pub(super) fn directory_refreshed(&mut self, canvas: CanvasId) {
        if let Some(scan) = &mut self.directory_scan {
            scan.baseline.remove(&canvas);
        }
    }
    pub(super) fn directory_page(
        &mut self,
        sequence: u64,
        after: Option<CanvasId>,
        limit: u32,
        items: &[wire::CanvasSummary],
        next: Option<CanvasId>,
    ) -> Result<()> {
        let prior = self.directory_scan.take();
        let mut scan = if after.is_none() {
            if self.directory_start != Some(sequence) {
                return Ok(());
            }
            self.cached_directory = None;
            Scan { baseline: self.queue.directory_grants(self.lease()?)?, present: BTreeSet::new(), next: None, request: None }
        } else {
            let Some(scan) = prior else { return Ok(()) };
            if scan.next != after || scan.request != Some(sequence) {
                return Ok(());
            }
            scan
        };
        if limit == 0 || limit > wire::MAX_PAGE || items.len() > limit as usize {
            return Err(other("Canvas directory page exceeds its requested limit"));
        }
        let mut previous = after;
        for item in items {
            if previous.is_some_and(|id| item.canvas <= id) || item.epoch == 0 || item.epoch > i64::MAX as u64 || item.revision > i64::MAX as u64 {
                return Err(other("Invalid canvas directory page identity or ordering"));
            }
            photocraft_scene::validate_name(&item.name).map_err(other)?;
            photocraft_scene::validate_name(&item.initial_name).map_err(other)?;
            if scan.present.len() == MAX_DIRECTORY || !scan.present.insert(item.canvas) {
                return Err(other("Canvas directory exceeds its reconciliation limit or repeats an identity"));
            }
            previous = Some(item.canvas);
        }
        if next.is_some() && (items.is_empty() || next != previous) {
            return Err(other("Canvas directory has an invalid continuation cursor"));
        }
        if next.is_some() {
            scan.next = next;
            scan.request = None;
            self.directory_scan = Some(scan);
            return Ok(());
        }
        let lease = self.lease()?.clone();
        let removed = self.queue.reconcile_directory(&lease, &scan.baseline, &scan.present)?;
        self.replicas.retain(|canvas, _| !removed.contains(canvas));
        self.outbound.retain(|event| !matches!(event, Event::State { info, .. } if removed.contains(&info.canvas)));
        // Fresh directory pages already replace the gallery. Only the selected
        // canvas needs an extra presentation invalidation, keeping output bounded.
        if let Some(canvas) = self.selected.filter(|canvas| removed.contains(canvas)) {
            self.selected = None;
            self.outbound.push_back(Event::Revoked { canvas });
        }
        Ok(())
    }
}
