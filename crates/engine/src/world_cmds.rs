//! Stable shared-world commands. Identity comes exclusively from the active
//! authenticated worker; invitations and artwork never enter the editor journal.
use photocraft_scene::shared::{DeviceId, Edit, MediaId, OpId, WorldState};
use photocraft_scene::world_protocol as wire;
use photocraft_scene::{CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always};
use crate::{EngineError, Result, Session};

mod assets;
mod lifecycle;
mod navigation;
mod playback;
mod stroke_copy;
pub use assets::{parse_export, parse_restore};

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError::Other(value.to_string())
}
fn available(s: &Session) -> std::result::Result<(), String> {
    if !s.worlds.available() {
        return Err("Shared canvases require native storage".into());
    }
    if !s.worlds.connected() {
        return Err("Connect your profile to the private service first".into());
    }
    Ok(())
}
fn opened(s: &Session) -> std::result::Result<(), String> {
    available(s)?;
    if s.worlds.active.is_none() {
        return Err("Open a shared canvas first".into());
    }
    Ok(())
}
fn active(s: &Session) -> Result<&crate::worlds::Canvas> {
    s.worlds.active.as_ref().ok_or_else(|| error("Open a shared canvas first"))
}
fn identity(s: &Session) -> Result<(OwnerId, DeviceId)> {
    s.worlds.account.as_ref().map(|a| (a.profile, a.device)).ok_or_else(|| error("Connect your profile first"))
}
fn owner(s: &Session) -> Result<()> {
    if active(s)?.info.owner != identity(s)?.0 {
        return Err(error("Only this canvas's owner can change its name or members"));
    }
    Ok(())
}
fn same(s: &Session, canvas: CanvasId) -> Result<()> {
    if active(s)?.info.canvas != canvas {
        return Err(error("This request belongs to another canvas"));
    }
    Ok(())
}
fn empty(p: &Value) -> Result<()> {
    if p.is_null() || p.as_object().is_some_and(|o| o.is_empty()) { Ok(()) } else { Err(error("This world command takes no parameters")) }
}
fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T> {
    // Check the already-parsed control payload before serde allocates owned arrays.
    let mut stack = vec![(value, 0usize)];
    let mut count = 0usize;
    let mut bytes = 0usize;
    while let Some((v, depth)) = stack.pop() {
        count = count.saturating_add(1);
        if depth > 24 || count > 65536 || bytes > wire::MAX_REQUEST_BYTES {
            return Err(error("World command exceeds its bounded parameter budget"));
        }
        match v {
            Value::String(s) => {
                if s.len() > 8192 {
                    return Err(error("World parameter string is too long"));
                }
                bytes = bytes.saturating_add(s.len());
            }
            Value::Array(a) => {
                if a.len() > photocraft_scene::MAX_POINTS {
                    return Err(error("World parameter array is too long"));
                }
                stack.extend(a.iter().map(|v| (v, depth + 1)));
            }
            Value::Object(o) => {
                if o.len() > 32 {
                    return Err(error("Too many world parameter fields"));
                }
                for (key, v) in o {
                    bytes = bytes.saturating_add(key.len());
                    stack.push((v, depth + 1));
                }
            }
            _ => {}
        }
    }
    T::deserialize(value).map_err(|e| error(format!("Invalid world parameters: {e}")))
}
fn fresh() -> Result<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(error)?;
        if bytes.iter().all(|b| *b == 0) {
            return Err(error("Could not allocate a nonzero world identity"));
        }
        Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }
    #[cfg(target_arch = "wasm32")]
    {
        Err(error("Shared worlds require the native app"))
    }
}
fn utc_ms() -> Result<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        u64::try_from(chrono::Utc::now().timestamp_millis()).map_err(error)
    }
    #[cfg(target_arch = "wasm32")]
    {
        Err(error("Shared invitations require the native app"))
    }
}
fn pending(s: &mut Session, action: wire::Action) -> Result<Value> {
    let sequence = s.world_request(action)?;
    Ok(json!({"pending":true,"accepted":false,"request":sequence}))
}
fn edit(s: &mut Session, action: Edit) -> Result<Value> {
    s.world_edit(active(s)?.info.canvas, action)?;
    Ok(json!({"pending":true,"saved":false}))
}
type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;
fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run) -> CommandSpec {
    CommandSpec { id, label, params, enabled, run, menu: &[], shortcut: None, journal: false }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Name {
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    canvas: CanvasId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rename {
    canvas: CanvasId,
    name: String,
    expected_revision: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invite {
    recipient: OwnerId,
    expires_in_ms: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Accept {
    canvas: CanvasId,
    id: OpId,
    secret: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    member: OwnerId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Draw {
    canvas: CanvasId,
    id: Option<StrokeId>,
    anchor: RegionKey,
    points: Vec<[f64; 2]>,
    width: f64,
    color: Srgba,
    #[serde(default)]
    erase: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Presence {
    camera: photocraft_scene::Camera,
    preview: Vec<[f64; 2]>,
    cursor: Option<photocraft_scene::Point>,
    stroke: Option<wire::PreviewStyle>,
}

#[derive(Clone, Copy)]
enum Target {
    Stroke(StrokeId),
    Media(MediaId),
}
fn history_target(world: &WorldState, profile: OwnerId, device: DeviceId, redo: bool) -> Option<Target> {
    let strokes = world
        .strokes
        .values()
        .filter(|s| s.device == device && s.stroke.owner == profile)
        .map(|s| (Target::Stroke(s.stroke.id), s.visible, s.created_revision, s.changed_revision));
    let media = world
        .media
        .values()
        .filter(|m| m.device == device && m.media.owner == profile)
        .map(|m| (Target::Media(m.media.id), m.visible, m.created_revision, m.changed_revision));
    let own: Vec<_> = strokes.chain(media).collect();
    let latest_created = own.iter().map(|(_, _, created, _)| *created).max().unwrap_or(0);
    own.into_iter()
        .filter(|(_, visible, _, changed)| *visible != redo && (!redo || *changed > latest_created))
        .max_by_key(|(_, _, _, changed)| *changed)
        .map(|(target, _, _, _)| target)
}
fn undo_redo(s: &mut Session, p: &Value, redo: bool) -> Result<Value> {
    empty(p)?;
    if s.worlds.busy() {
        return Err(error("Wait for the previous local save"));
    }
    let (profile, device) = identity(s)?;
    let target = history_target(&active(s)?.display, profile, device, redo)
        .ok_or_else(|| error(if redo { "Nothing on this device is ready to redo" } else { "Nothing on this device is ready to undo" }))?;
    edit(
        s,
        match target {
            Target::Stroke(stroke) => Edit::SetStrokeVisible { stroke, visible: redo },
            Target::Media(media) => Edit::SetMediaVisible { media, visible: redo },
        },
    )
}

pub fn specs() -> Vec<CommandSpec> {
    let mut specs = vec![
        spec("komaphone.world.status", "Shared canvas status", "{}", always, |s, p| {
            empty(p)?;
            s.poll_worlds();
            let canvas = s.worlds.active.as_ref().map(|a| {
                json!({"info":a.info,"pending":a.pending,"canonical_revision":a.canonical.revision,
                "display_revision":a.display.revision,"stroke_count":a.display.strokes.len(),"media_count":a.display.media.len(),
                "tours":a.display.tours.values().map(|tour|json!({"id":tour.id,"name":tour.name,"revision":tour.revision,"stops":tour.stops.len()})).collect::<Vec<_>>(),"viewer":a.viewer,"notice":a.notice})
            });
            Ok(json!({"available":s.worlds.available(),"connected":s.worlds.connected(),"busy":s.worlds.busy(),"account":s.worlds.account,
                "requests_pending":s.worlds.requests_pending(),"presence_requests_pending":s.worlds.presence_requests_pending(),"flight":s.worlds.flight.as_ref().map(|flight|json!({"from":flight.from,"to":flight.to,"elapsed":flight.elapsed,"duration":flight.duration,"paused":flight.paused})),"invitation_id":s.worlds.invitation.as_ref().and_then(|value|value.get("id")),
                "active":canvas,"items":s.worlds.items,"trash":s.worlds.trash,"people":s.worlds.people,"members":s.worlds.members,"peers":s.worlds.peers,"dropped_previews":s.worlds.dropped_previews,
                "error":s.worlds.error,"notice":s.worlds.notice,"state":s.worlds.status(),"export":s.worlds.assets.export,"media_error":s.worlds.assets.error,
                "recovery":s.worlds.recovery.iter().map(|item|json!({"id":item.operation.id,"canvas":item.operation.canvas,"reason":item.reason})).collect::<Vec<_>>(),"recovery_next":s.worlds.recovery_next,
                "prepared_media":s.worlds.assets.prepared().map(|media|json!({"frames":media.frames.len(),"looping":media.looping,"provenance":media.provenance,"dimensions":media.frames.first().map(|frame|[frame.width,frame.height])}))}))
        }),
        spec("komaphone.world.list", "Refresh shared canvases", "{}", available, |s, p| {
            empty(p)?;
            pending(s, wire::Action::List { after: None, limit: wire::MAX_PAGE })
        }),
        spec("komaphone.world.people", "Find service members", "{}", available, |s, p| {
            empty(p)?;
            pending(s, wire::Action::People { after: None, limit: wire::MAX_PAGE })
        }),
        spec("komaphone.world.create", "Create shared canvas", r#"{"name":string}"#, available, |s, p| {
            let p: Name = parse(p)?;
            photocraft_scene::validate_name(&p.name).map_err(error)?;
            let canvas = CanvasId::new(&fresh()?).map_err(error)?;
            s.world_create(canvas, p.name)?;
            Ok(json!({"pending":true,"accepted":false,"canvas":canvas}))
        }),
        spec("komaphone.world.open", "Open shared canvas", r#"{"canvas":id}"#, available, |s, p| {
            let p: Open = parse(p)?;
            s.world_open(p.canvas)?;
            Ok(json!({"pending":true}))
        }),
        spec("komaphone.world.rename", "Rename shared canvas", r#"{"canvas":id,"name":string,"expected_revision":integer?}"#, opened, |s, p| {
            let p: Rename = parse(p)?;
            same(s, p.canvas)?;
            owner(s)?;
            photocraft_scene::validate_name(&p.name).map_err(error)?;
            let expected_revision = p.expected_revision.unwrap_or(active(s)?.display.metadata_revision);
            edit(s, Edit::Rename { name: p.name, expected_revision })
        }),
        spec("komaphone.world.members", "Canvas members", "{}", opened, |s, p| {
            empty(p)?;
            pending(s, wire::Action::Members { canvas: active(s)?.info.canvas, after: None, limit: wire::MAX_PAGE })
        }),
        spec(
            "komaphone.world.invite",
            "Invite to shared canvas",
            r#"{"recipient":profile_id,"expires_in_ms":integer=86400000(max604800000)}"#,
            opened,
            |s, p| {
                let p: Invite = parse(p)?;
                owner(s)?;
                if p.recipient == identity(s)?.0 {
                    return Err(error("The owner already belongs to this canvas"));
                }
                let duration = p.expires_in_ms.unwrap_or(86400000);
                if !(1..=604800000).contains(&duration) {
                    return Err(error("Invitation duration must be within seven days"));
                }
                let now = utc_ms()?;
                let expires_at_ms = now.checked_add(duration).ok_or_else(|| error("Invitation expiry overflow"))?;
                let canvas = active(s)?.info.canvas;
                let id = OpId::new(&fresh()?).map_err(error)?;
                let secret = format!("{}{}", fresh()?, fresh()?);
                let sequence = s.world_request(wire::Action::Invite { canvas, id, recipient: p.recipient, secret: secret.clone(), expires_at_ms })?;
                Ok(json!({"pending":true,"accepted":false,"request":sequence}))
            },
        ),
        spec("komaphone.world.accept", "Accept canvas invitation", r#"{"canvas":id,"id":invitation_id,"secret":capability}"#, available, |s, p| {
            let p: Accept = parse(p)?;
            photocraft_party::protocol::validate_secret(&p.secret).map_err(error)?;
            pending(s, wire::Action::Accept { canvas: p.canvas, id: p.id, secret: p.secret })
        }),
        spec("komaphone.world.remove", "Remove canvas member", r#"{"member":profile_id}"#, opened, |s, p| {
            let p: Member = parse(p)?;
            owner(s)?;
            if p.member == identity(s)?.0 {
                return Err(error("The owner cannot be removed"));
            }
            pending(s, wire::Action::Remove { canvas: active(s)?.info.canvas, member: p.member })
        }),
        spec("komaphone.world.leave", "Leave shared canvas", "{}", opened, |s, p| {
            empty(p)?;
            if identity(s)?.0 == active(s)?.info.owner {
                return Err(error("The canvas owner cannot leave"));
            }
            pending(s, wire::Action::Leave { canvas: active(s)?.info.canvas })
        }),
        spec(
            "komaphone.world.stroke",
            "Draw on shared canvas",
            r#"{"canvas":id,"id":stroke_id?,"anchor":region,"points":[[x,y]],"width":diameter,"color":{"rgba":[r,g,b,1]},"erase":false}"#,
            opened,
            |s, p| {
                if p.get("points").and_then(Value::as_array).is_none_or(|p| p.is_empty() || p.len() > photocraft_scene::MAX_POINTS) {
                    return Err(error("A world stroke needs 1–8192 samples"));
                }
                let p: Draw = parse(p)?;
                same(s, p.canvas)?;
                let stroke = Stroke {
                    id: match p.id {
                        Some(id) => id,
                        None => StrokeId::new(&fresh()?).map_err(error)?,
                    },
                    canvas: p.canvas,
                    owner: identity(s)?.0,
                    revision: 0,
                    anchor: p.anchor,
                    points: p.points,
                    width: p.width,
                    color: p.color,
                    erase: p.erase,
                };
                stroke.validate().map_err(error)?;
                photocraft_scene::render::spatial_cells(&stroke, photocraft_scene::MAX_SPATIAL_CELLS).map_err(error)?;
                s.worlds.tour_playback = None;
                edit(s, Edit::AddStroke { stroke })
            },
        ),
        spec("komaphone.world.undo", "Undo my device's last edit", "{}", opened, |s, p| undo_redo(s, p, false)),
        spec("komaphone.world.redo", "Redo my device's last edit", "{}", opened, |s, p| undo_redo(s, p, true)),
        spec("komaphone.world.peers", "Shared canvas presence", "{}", opened, |s, p| {
            empty(p)?;
            pending(s, wire::Action::Peers { canvas: active(s)?.info.canvas })
        }),
        spec(
            "komaphone.world.presence",
            "Publish my transient canvas presence",
            r#"{"camera":camera,"preview":[[x,y]],"cursor":point?,"stroke":preview_style?}"#,
            opened,
            |s, p| {
                if p.get("preview").and_then(Value::as_array).is_none_or(|a| a.len() > wire::MAX_PREVIEW_POINTS) {
                    return Err(error("Presence preview exceeds 128 samples"));
                }
                let p: Presence = parse(p)?;
                same(s, p.camera.canvas)?;
                photocraft_scene::coordinates::validate_camera(&p.camera).map_err(error)?;
                if p.preview.iter().flatten().any(|n| !n.is_finite() || n.abs() > 8192.) {
                    return Err(error("Invalid presence sample"));
                }
                if let Some(cursor) = p.cursor {
                    photocraft_scene::coordinates::validate_point(cursor).map_err(error)?;
                }
                if let Some(style) = &p.stroke {
                    let stroke = Stroke {
                        id: style.id,
                        canvas: p.camera.canvas,
                        owner: identity(s)?.0,
                        revision: 0,
                        anchor: style.anchor,
                        points: if p.preview.is_empty() { vec![[0., 0.]] } else { p.preview.clone() },
                        width: style.width,
                        color: style.color,
                        erase: style.erase,
                    };
                    stroke.validate().map_err(error)?;
                }
                pending(
                    s,
                    wire::Action::Presence {
                        canvas: p.camera.canvas,
                        epoch: active(s)?.info.epoch,
                        camera: p.camera,
                        preview: p.preview,
                        cursor: p.cursor,
                        stroke: p.stroke,
                    },
                )
            },
        ),
    ];
    specs.extend(navigation::specs());
    specs.extend(playback::specs());
    specs.extend(assets::specs());
    specs.extend(lifecycle::specs());
    specs.extend(stroke_copy::specs());
    specs
}

#[cfg(test)]
mod tests;
