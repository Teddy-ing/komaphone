use super::{CommandSpec, Deserialize, Name, Result, Session, Value, active, empty, error, json, opened, parse, same, spec};
use photocraft_scene::{Bookmark, Camera, coordinates};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetCamera {
    camera: Camera,
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
struct FlightControl {
    state: String,
}

pub(super) fn save(s: &mut Session, viewer: crate::worlds::Viewer) -> Result<Value> {
    let old = active(s)?.viewer.clone();
    s.worlds.active.as_mut().ok_or_else(|| error("Open a shared canvas first"))?.viewer = viewer;
    if let Err(error) = s.world_save_view() {
        if let Some(active) = &mut s.worlds.active {
            active.viewer = old;
        }
        return Err(error);
    }
    Ok(json!({"pending":true,"saved":false}))
}
pub(super) fn camera(s: &mut Session, camera: Camera) -> Result<Value> {
    same(s, camera.canvas)?;
    coordinates::validate_camera(&camera).map_err(error)?;
    let mut viewer = active(s)?.viewer.clone();
    viewer.camera = camera;
    let result = save(s, viewer)?;
    s.worlds.tour_playback = None;
    s.worlds.flight = None;
    Ok(result)
}
pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        spec("komaphone.world.camera", "Navigate shared canvas", r#"{"camera":exact_canvas_camera}"#, opened, |s, p| {
            let p: SetCamera = parse(p)?;
            camera(s, p.camera)
        }),
        spec("komaphone.world.home", "Shared canvas home", "{}", opened, |s, p| {
            empty(p)?;
            camera(s, coordinates::home(active(s)?.info.canvas))
        }),
        spec("komaphone.world.bookmark", "Bookmark my canvas location", r#"{"name":string}"#, opened, |s, p| {
            let p: Name = parse(p)?;
            photocraft_scene::validate_name(&p.name).map_err(error)?;
            let mut viewer = active(s)?.viewer.clone();
            let bookmark = Bookmark { name: p.name, camera: viewer.camera.clone() };
            if let Some(old) = viewer.bookmarks.iter_mut().find(|old| old.name == bookmark.name) {
                *old = bookmark;
            } else {
                viewer.bookmarks.push(bookmark);
            }
            if viewer.bookmarks.len() > photocraft_scene::MAX_BOOKMARKS {
                return Err(error("Bookmark limit reached"));
            }
            save(s, viewer)
        }),
        spec("komaphone.world.go", "Go to my canvas bookmark", r#"{"name":string,"animate":bool=false}"#, opened, |s, p| {
            let p: Go = parse(p)?;
            let target = active(s)?.viewer.bookmarks.iter().find(|b| b.name == p.name).ok_or_else(|| error("Bookmark was not found"))?.camera.clone();
            if p.animate {
                let from = active(s)?.viewer.camera.clone();
                coordinates::flight(&from, &target, 0.5).map_err(error)?;
                s.worlds.tour_playback = None;
                s.worlds.flight = Some(crate::canvases::Flight { from, to: target, elapsed: 0.0, duration: 2.0, paused: false });
                return Ok(json!({"playing":true}));
            }
            camera(s, target)
        }),
        spec("komaphone.world.flight", "Control bookmark auto zoom", r#"{"state":"pause|resume|cancel"}"#, opened, |s, p| {
            let p: FlightControl = parse(p)?;
            match p.state.as_str() {
                "cancel" => s.worlds.flight = None,
                "pause" | "resume" => {
                    let flight = s.worlds.flight.as_mut().ok_or_else(|| error("No active auto zoom"))?;
                    flight.paused = p.state == "pause";
                }
                _ => return Err(error("Choose pause, resume or cancel")),
            }
            s.world_save_view()?;
            Ok(json!({"pending":true}))
        }),
        spec("komaphone.world.bookmark.remove", "Remove my canvas bookmark", r#"{"name":string}"#, opened, |s, p| {
            let p: Name = parse(p)?;
            let mut viewer = active(s)?.viewer.clone();
            if !viewer.bookmarks.iter().any(|b| b.name == p.name) {
                return Err(error("Bookmark was not found"));
            }
            viewer.bookmarks.retain(|b| b.name != p.name);
            save(s, viewer)
        }),
    ]
}
