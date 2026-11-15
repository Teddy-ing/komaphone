use super::{Session, validation, wire};

impl Session {
    pub(super) fn world_response(&mut self, sequence: u64, response: wire::Response) {
        let request = self.worlds.requests.remove(&sequence);
        if let Err(error) = validation::response(&response, request.as_ref(), sequence) {
            eprintln!(
                "shared canvas response rejected: sequence={}, request_kind={:?}, response_kind={:?}",
                sequence,
                request.as_ref().map(std::mem::discriminant),
                std::mem::discriminant(&response)
            );
            self.world_asset_error(sequence, &error.to_string());
            self.worlds.error = Some(error.to_string());
            self.worlds.auto_open.remove(&sequence);
            return;
        }
        self.world_asset_response(sequence, request.as_ref(), &response);
        match response {
            wire::Response::Invited { id, .. } => {
                if let Some(wire::Action::Invite { canvas, id: expected, secret, .. }) = request
                    && id == expected
                {
                    self.worlds.invitation = Some(serde_json::json!({"canvas":canvas,"id":id,"secret":secret}));
                }
            }
            wire::Response::Done => {
                if matches!(request, Some(wire::Action::Delete { .. })) {
                    let _ = self.world_request(wire::Action::Trash { after: None, limit: wire::MAX_PAGE });
                    self.worlds.notice = Some("Canvas moved to Trash. Its owner can restore the artwork and previous member list.".into());
                }
                if let Some(wire::Action::Remove { canvas, .. }) = request
                    && self.worlds.selected == Some(canvas)
                {
                    let _ = self.world_request(wire::Action::Members { canvas, after: None, limit: wire::MAX_PAGE });
                }
            }
            wire::Response::Trash { items, next } => self.world_trash_response(request, items, next),
            wire::Response::Canvases { items, next } => {
                self.worlds.fresh_catalog = true;
                if matches!(request, Some(wire::Action::List { after: None, .. })) || sequence == 0 {
                    self.worlds.items.clear();
                }
                if self.worlds.items.len().saturating_add(items.len()) > 16_384 {
                    self.worlds.error = Some("Canvas directory exceeds the local display limit".into());
                    return;
                }
                for item in items {
                    if let Some(old) = self.worlds.items.iter_mut().find(|old| old.canvas == item.canvas) {
                        *old = item;
                    } else {
                        self.worlds.items.push(item);
                    }
                }
                self.thumbnail_catalog_changed();
                if let Some(after) = next {
                    let _ = self.world_request(wire::Action::List { after: Some(after), limit: wire::MAX_PAGE });
                }
            }
            wire::Response::People { items, next } => {
                if matches!(request, Some(wire::Action::People { after: None, .. })) {
                    self.worlds.people.clear();
                }
                if self.worlds.people.len().saturating_add(items.len()) > 65_536 {
                    self.worlds.error = Some("People directory exceeds the local display limit".into());
                    return;
                }
                for item in items {
                    match self.worlds.people.binary_search_by_key(&item.profile, |person| person.profile) {
                        Ok(index) => {
                            if let Some(person) = self.worlds.people.get_mut(index) {
                                *person = item;
                            }
                        }
                        Err(index) => self.worlds.people.insert(index, item),
                    }
                }
                if let Some(after) = next {
                    let _ = self.world_request(wire::Action::People { after: Some(after), limit: wire::MAX_PAGE });
                }
            }
            wire::Response::Members { items, next } => {
                if let Some(wire::Action::Members { canvas, after, .. }) = request
                    && self.worlds.selected == Some(canvas)
                {
                    if after.is_none() {
                        self.worlds.members.clear();
                    }
                    if self.worlds.members.len().saturating_add(items.len()) > 65_536 {
                        self.worlds.error = Some("Member directory exceeds the local display limit".into());
                        return;
                    }
                    self.worlds.members.extend(items);
                    if let Some(after) = next {
                        let _ = self.world_request(wire::Action::Members { canvas, after: Some(after), limit: wire::MAX_PAGE });
                    }
                }
            }
            wire::Response::Canvas { info } => {
                if matches!(request, Some(wire::Action::RestoreOriginal { .. })) {
                    self.worlds.trash.retain(|item| item.canvas != info.canvas);
                }
                if self.worlds.auto_open.remove(&sequence) || matches!(request, Some(wire::Action::Accept { .. } | wire::Action::RestoreOriginal { .. })) {
                    let _ = self.world_open(info.canvas);
                }
                if let Some(old) = self.worlds.items.iter_mut().find(|old| old.canvas == info.canvas) {
                    *old = info;
                } else {
                    self.worlds.items.push(info);
                }
                self.thumbnail_catalog_changed();
            }
            wire::Response::Peers { peers } => {
                if let Some(wire::Action::Peers { canvas } | wire::Action::Presence { canvas, .. }) = request
                    && self.worlds.selected == Some(canvas)
                {
                    self.worlds.peers = peers;
                    self.worlds.peers_received = Some(web_time::Instant::now());
                }
            }
            _ => {}
        }
    }
    fn world_trash_response(&mut self, request: Option<wire::Action>, items: Vec<wire::TrashedCanvas>, next: Option<photocraft_scene::CanvasId>) {
        if matches!(request, Some(wire::Action::Trash { after: None, .. })) {
            self.worlds.trash.clear();
        }
        if self.worlds.trash.len().saturating_add(items.len()) > 16_384 {
            self.worlds.error = Some("Deleted canvas directory exceeds the local display limit".into());
            return;
        }
        for item in items {
            if let Some(previous) = self.worlds.trash.iter_mut().find(|previous| previous.canvas == item.canvas) {
                *previous = item;
            } else {
                self.worlds.trash.push(item);
            }
        }
        if let Some(after) = next {
            let _ = self.world_request(wire::Action::Trash { after: Some(after), limit: wire::MAX_PAGE });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_updates_peers_only_while_its_canvas_remains_selected() {
        let canvas = photocraft_scene::CanvasId::new("11111111111111111111111111111111").unwrap();
        let other = photocraft_scene::CanvasId::new("22222222222222222222222222222222").unwrap();
        let camera = photocraft_scene::coordinates::home(canvas);
        let request = wire::Action::Presence { canvas, epoch: 1, camera: camera.clone(), preview: vec![], cursor: None, stroke: None };
        let peer = wire::Peer {
            profile: photocraft_scene::OwnerId::new("33333333333333333333333333333333").unwrap(),
            device: photocraft_scene::shared::DeviceId::new("44444444444444444444444444444444").unwrap(),
            camera,
            preview: vec![],
            cursor: None,
            stroke: None,
            expires_at_ms: 2000,
        };
        let mut session = Session::new();
        session.worlds.selected = Some(canvas);
        session.worlds.requests.insert(7, request.clone());
        session.world_response(7, wire::Response::Peers { peers: vec![peer.clone()] });
        assert!(session.worlds.error.is_none());
        assert!(session.worlds.requests.is_empty());
        assert_eq!(session.worlds.peers.len(), 1);
        assert_eq!(session.worlds.peers[0].device, peer.device);
        assert!(session.worlds.peers_received.is_some());

        session.worlds.selected = Some(other);
        session.worlds.peers.clear();
        session.worlds.peers_received = None;
        session.worlds.requests.insert(8, request);
        session.world_response(8, wire::Response::Peers { peers: vec![peer] });
        assert!(session.worlds.error.is_none());
        assert!(session.worlds.requests.is_empty());
        assert!(session.worlds.peers.is_empty());
        assert!(session.worlds.peers_received.is_none());
    }
}
