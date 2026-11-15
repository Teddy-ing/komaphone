//! Stable single-user canvas commands; each request is scoped to the active identity.
use photocraft_scene::{Bookmark, Camera, CanvasId, RegionKey, Srgba, Stroke, StrokeId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::canvases::{Action, Flight};
use crate::commands::{CommandSpec, always};
use crate::{EngineError, Result, Session};

fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn available(s: &Session) -> std::result::Result<(), String> {
    if s.canvases.available() { Ok(()) } else { Err("Local canvas storage is unavailable".into()) }
}
fn undoable(s: &Session) -> std::result::Result<(), String> {
    available(s)?;
    if !s.canvases.busy() && s.canvases.active.as_ref().is_some_and(|a| a.undo_count > 0) {
        Ok(())
    } else {
        Err("No saved canvas stroke is ready to undo".into())
    }
}
fn redoable(s: &Session) -> std::result::Result<(), String> {
    available(s)?;
    if !s.canvases.busy() && s.canvases.active.as_ref().is_some_and(|a| a.redo_count > 0) { Ok(()) } else { Err("No canvas stroke is ready to redo".into()) }
}
fn parse<T: serde::de::DeserializeOwned>(p: &Value) -> Result<T> {
    if p.get("name").and_then(Value::as_str).is_some_and(|name| name.len() > photocraft_scene::MAX_NAME_BYTES) {
        return Err(error("Canvas names are limited to 96 UTF-8 bytes"));
    }
    T::deserialize(p).map_err(|e| error(format!("Invalid canvas request: {e}")))
}
fn empty(p: &Value) -> Result<()> {
    if p.is_null() || p.as_object().is_some_and(|o| o.is_empty()) { Ok(()) } else { Err(error("This canvas command takes no parameters")) }
}
fn pending(s: &mut Session, action: Action) -> Result<Value> {
    s.canvas_action(action)?;
    Ok(json!({"pending":true,"saved":false}))
}
fn active(s: &Session) -> Result<&photocraft_scene::CanvasInfo> {
    s.canvases.active.as_ref().ok_or_else(|| error("Open a canvas first"))
}
fn camera(s: &Session) -> Result<Camera> {
    s.canvases.camera.clone().ok_or_else(|| error("Open a canvas first"))
}
fn id() -> Result<StrokeId> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| error(format!("Cannot allocate a stroke identity: {e}")))?;
        StrokeId::new(&bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()).map_err(|e| error(e.to_string()))
    }
    #[cfg(target_arch = "wasm32")]
    {
        Err(error("Local canvas persistence is available in the native app"))
    }
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
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetCamera {
    camera: Camera,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Draw {
    canvas: CanvasId,
    anchor: RegionKey,
    points: Vec<[f64; 2]>,
    width: f64,
    color: Srgba,
    #[serde(default)]
    erase: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Go {
    name: String,
    #[serde(default)]
    animate: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FlightState {
    state: String,
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "komaphone.canvas.status",
            label: "Canvas status",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            journal: false,
            run: |s, p| {
                empty(p)?;
                s.poll_canvases();
                Ok(
                    json!({"available":s.canvases.available(),"busy":s.canvases.busy(),"pending":s.canvases.has_pending(),"items":s.canvases.items,"active":s.canvases.active,"camera":s.canvases.camera,"flight":s.canvases.flight,"view":s.canvases.view.as_ref().map(|v|json!({"viewport":v.viewport,"revision":v.revision,"stats":v.stats})),"pendingStrokes":s.canvases.pending_strokes.len(),"error":s.canvases.error,"notice":s.canvases.notice,"usageBytes":s.canvases.usage_bytes,"quotaBytes":s.canvases.quota_bytes,"state":s.canvases.status_text()}),
                )
            },
        },
        CommandSpec {
            id: "komaphone.canvas.list",
            label: "Refresh canvases",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: available,
            journal: false,
            run: |s, p| {
                empty(p)?;
                pending(s, Action::List)
            },
        },
        CommandSpec {
            id: "komaphone.canvas.view",
            label: "Inspect loaded canvas artwork",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: available,
            journal: false,
            run: |s, p| {
                empty(p)?;
                s.poll_canvases();
                let view = s.canvases.view.as_ref().ok_or_else(|| error("Canvas view is still loading"))?;
                if Some(&view.viewport.camera) != s.canvases.camera.as_ref() || active(s)?.revision != view.revision {
                    return Err(error("Canvas view is still loading"));
                }
                let mut hashes = Vec::with_capacity(view.strokes.len());
                let mut combined = blake3::Hasher::new();
                for stroke in &view.strokes {
                    let bytes = serde_json::to_vec(stroke).map_err(|e| error(e.to_string()))?;
                    let hash = blake3::hash(&bytes);
                    combined.update(hash.as_bytes());
                    hashes.push(json!({"id":stroke.id,"revision":stroke.revision,"depth":stroke.anchor.depth,"samples":stroke.points.len(),"blake3":hash.to_hex().to_string()}));
                }
                Ok(
                    json!({"canvas":view.viewport.camera.canvas,"revision":view.revision,"viewport":view.viewport,"stats":view.stats,"strokes":hashes,"contentHash":combined.finalize().to_hex().to_string()}),
                )
            },
        },
        CommandSpec {
            id: "komaphone.canvas.create",
            label: "Create canvas",
            menu: &[],
            shortcut: None,
            params: r#"{"name":string}"#,
            enabled: available,
            journal: false,
            run: |s, p| {
                let p: Name = parse(p)?;
                photocraft_scene::validate_name(&p.name).map_err(|e| error(e.to_string()))?;
                pending(s, Action::Create { name: p.name, previous: s.canvases.camera.clone() })
            },
        },
        CommandSpec {
            id: "komaphone.canvas.open",
            label: "Open canvas",
            menu: &[],
            shortcut: None,
            params: r#"{"canvas":id}"#,
            enabled: available,
            journal: false,
            run: |s, p| {
                let p: Open = parse(p)?;
                pending(s, Action::Open { canvas: p.canvas, previous: s.canvases.camera.clone() })
            },
        },
        CommandSpec {
            id: "komaphone.canvas.rename",
            label: "Rename canvas",
            menu: &[],
            shortcut: None,
            params: r#"{"canvas":id,"name":string}"#,
            enabled: available,
            journal: false,
            run: |s, p| {
                let p: Rename = parse(p)?;
                photocraft_scene::validate_name(&p.name).map_err(|e| error(e.to_string()))?;
                if active(s)?.id != p.canvas {
                    return Err(error("Rename targets the active canvas"));
                }
                pending(s, Action::Rename { canvas: p.canvas, name: p.name })
            },
        },
        CommandSpec {
            id: "komaphone.canvas.camera",
            label: "Navigate canvas",
            menu: &[],
            shortcut: None,
            params: r#"{"camera":exact_canvas_camera}"#,
            enabled: available,
            journal: false,
            run: |s, p| {
                let p: SetCamera = parse(p)?;
                s.canvases.flight = None;
                s.canvas_set_camera(p.camera)?;
                Ok(json!({"pending":true,"saved":false}))
            },
        },
        CommandSpec {
            id: "komaphone.canvas.stroke",
            label: "Draw on canvas",
            menu: &[],
            shortcut: None,
            params: r#"{"canvas":id,"anchor":region,"points":[[x,y]],"width":local_diameter,"color":{"rgba":[r,g,b,a]},"erase":false}"#,
            enabled: available,
            journal: false,
            run: |s, p| {
                // Inspect the JSON array before cloning/deserializing an adversarial payload.
                if p.get("points").and_then(Value::as_array).is_none_or(|v| v.is_empty() || v.len() > photocraft_scene::MAX_POINTS) {
                    return Err(error("Canvas stroke needs 1–8192 samples"));
                }
                let p: Draw = parse(p)?;
                let owner = active(s)?.owner;
                let stroke = Stroke {
                    id: id()?,
                    canvas: p.canvas,
                    owner,
                    revision: 0,
                    anchor: p.anchor,
                    points: p.points,
                    width: p.width,
                    color: p.color,
                    erase: p.erase,
                };
                stroke.validate().map_err(|e| error(e.to_string()))?;
                s.canvases.flight = None;
                pending(s, Action::Stroke(stroke))
            },
        },
        CommandSpec {
            id: "komaphone.canvas.home",
            label: "Canvas home",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: available,
            journal: false,
            run: |s, p| {
                empty(p)?;
                let target = photocraft_scene::coordinates::home(active(s)?.id);
                s.canvases.flight = None;
                s.canvas_set_camera(target)?;
                Ok(json!({"pending":true,"saved":false}))
            },
        },
        CommandSpec {
            id: "komaphone.canvas.bookmark",
            label: "Bookmark canvas location",
            menu: &[],
            shortcut: None,
            params: r#"{"name":string}"#,
            enabled: available,
            journal: false,
            run: |s, p| {
                if s.canvases.busy() {
                    return Err(error("Wait for the previous canvas change to save before saving another bookmark"));
                }
                let p: Name = parse(p)?;
                photocraft_scene::validate_name(&p.name).map_err(|e| error(e.to_string()))?;
                let current = active(s)?;
                let canvas = current.id;
                let mut bookmarks = current.bookmarks.clone();
                let bookmark = Bookmark { name: p.name, camera: camera(s)? };
                if let Some(existing) = bookmarks.iter_mut().find(|b| b.name == bookmark.name) {
                    *existing = bookmark;
                } else {
                    bookmarks.push(bookmark);
                }
                if bookmarks.len() > photocraft_scene::MAX_BOOKMARKS {
                    return Err(error("Canvas bookmark limit reached"));
                }
                pending(s, Action::Bookmarks { canvas, bookmarks })
            },
        },
        CommandSpec {
            id: "komaphone.canvas.go",
            label: "Go to bookmark",
            menu: &[],
            shortcut: None,
            params: r#"{"name":string,"animate":false}"#,
            enabled: available,
            journal: false,
            run: |s, p| {
                if s.canvases.switching() {
                    return Err(error("Wait for the canvas switch to finish"));
                }
                let p: Go = parse(p)?;
                let to = active(s)?.bookmarks.iter().find(|b| b.name == p.name).map(|b| b.camera.clone()).ok_or_else(|| error("Bookmark was not found"))?;
                if p.animate {
                    s.canvases.flight = Some(Flight { from: camera(s)?, to, elapsed: 0.0, duration: 3.0, paused: false });
                } else {
                    s.canvases.flight = None;
                    s.canvas_set_camera(to)?;
                }
                Ok(json!({"pending":true,"saved":false}))
            },
        },
        CommandSpec {
            id: "komaphone.canvas.flight",
            label: "Control automatic zoom",
            menu: &[],
            shortcut: None,
            params: r#"{"state":"pause|resume|cancel"}"#,
            enabled: available,
            journal: false,
            run: |s, p| {
                let p: FlightState = parse(p)?;
                match p.state.as_str() {
                    "cancel" => s.canvases.flight = None,
                    "pause" | "resume" => {
                        let flight = s.canvases.flight.as_mut().ok_or_else(|| error("No automatic zoom is active"))?;
                        flight.paused = p.state == "pause";
                    }
                    _ => return Err(error("Unknown automatic zoom action")),
                }
                Ok(json!({"ok":true}))
            },
        },
        CommandSpec {
            id: "komaphone.canvas.undo",
            label: "Undo canvas stroke",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: undoable,
            journal: false,
            run: |s, p| {
                empty(p)?;
                undoable(s).map_err(error)?;
                pending(s, Action::Undo(active(s)?.id))
            },
        },
        CommandSpec {
            id: "komaphone.canvas.redo",
            label: "Redo canvas stroke",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: redoable,
            journal: false,
            run: |s, p| {
                empty(p)?;
                redoable(s).map_err(error)?;
                pending(s, Action::Redo(active(s)?.id))
            },
        },
        CommandSpec {
            id: "komaphone.canvas.retry",
            label: "Retry canvas storage",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: available,
            journal: false,
            run: |s, p| {
                empty(p)?;
                s.retry_canvases()?;
                Ok(json!({"pending":true,"saved":false}))
            },
        },
    ]
}
