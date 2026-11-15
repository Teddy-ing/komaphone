//! Opt-in, bounded actual-screen pixel evidence for shared-canvas automation.
//! The harness owns the end-to-end clock. No PNG work occurs before the match reply.
use crate::{
    PhotocraftApp,
    control::{ControlResponse, Outcome},
    screenshot::{CaptureGeometry, MAX_IMAGE_BYTES},
};
use craft_ui::capture::CaptureSlot;
use egui::{ColorImage, Context, Rect};
use photocraft_engine::{
    scene::{self, CanvasId, OwnerId, StrokeId, Viewport, shared::SharedStroke},
    worlds::Account,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{Arc, mpsc::Sender},
    time::Duration,
};
use web_time::Instant;

const DEADLINE: Duration = Duration::from_secs(4);
const MAX_CAPTURES: u32 = 256;
const MAX_EXCLUDED: usize = 256;
type Reply = Sender<ControlResponse>;
type PixelSample = ([usize; 2], Vec<[u8; 4]>, usize);

#[derive(Clone, PartialEq)]
struct Subject {
    account: Account,
    canvas: CanvasId,
    epoch: u64,
    activation: u64,
    camera: scene::Camera,
    canonical: u64,
    display: u64,
}
impl Subject {
    fn current(app: &PhotocraftApp) -> Option<Self> {
        if !app.ui.world.open {
            return None;
        }
        let active = app.session.worlds.active.as_ref()?;
        if active.viewer.review.as_ref().is_some_and(|review| review.interval.is_some()) {
            return None;
        }
        Some(Self {
            account: app.session.worlds.account.clone()?,
            canvas: active.info.canvas,
            epoch: active.info.epoch,
            activation: app.session.worlds.activation,
            camera: active.viewer.camera.clone(),
            canonical: active.canonical.revision,
            display: active.display.revision,
        })
    }
    fn json(&self) -> Value {
        json!({"canvas":self.canvas,"profile":self.account.profile,"device":self.account.device,"epoch":self.epoch,
            "activation":self.activation,"camera":self.camera,"canonicalRevision":self.canonical,"displayRevision":self.display})
    }
}
struct Spec {
    canvas: CanvasId,
    sender: OwnerId,
    point: [f64; 2],
    excluded: BTreeSet<StrokeId>,
}
enum Request {
    Arm(Spec),
    Wait(u64),
    Save(u64, String),
    Cancel(u64),
}
fn parse(value: &Value) -> Result<Request, String> {
    let object = value.as_object().ok_or("World probe parameters must be an object")?;
    let text = |key: &str| object.get(key).and_then(Value::as_str).ok_or_else(|| format!("World probe {key} must be a string"));
    let action = text("action")?;
    let fields: &[&str] = match action {
        "arm" => &["action", "canvas", "sender", "x", "y", "excludedStrokes"],
        "wait" | "cancel" => &["action", "token"],
        "save" => &["action", "token", "path"],
        _ => return Err("World probe action must be arm, wait, save or cancel".into()),
    };
    if let Some(key) = object.keys().find(|key| !fields.contains(&key.as_str())) {
        return Err(format!("Unknown world probe field {key}"));
    }
    if action == "arm" {
        let canvas = CanvasId::new(text("canvas")?).map_err(|e| e.to_string())?;
        let sender = OwnerId::new(text("sender")?).map_err(|e| e.to_string())?;
        let number = |key: &str| {
            object
                .get(key)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && (0.0..=1_000_000.0).contains(v))
                .ok_or_else(|| format!("World probe {key} must be a finite screen coordinate"))
        };
        let point = [number("x")?, number("y")?];
        let values = object.get("excludedStrokes").and_then(Value::as_array).ok_or("World probe excludedStrokes must be an array")?;
        if values.len() > MAX_EXCLUDED {
            return Err("World probe has too many excluded strokes".into());
        }
        let excluded = values
            .iter()
            .map(|value| StrokeId::new(value.as_str().ok_or("Excluded stroke must be an ID")?).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        Ok(Request::Arm(Spec { canvas, sender, point, excluded }))
    } else {
        let token = object.get("token").and_then(Value::as_u64).filter(|id| *id > 0).ok_or("World probe token must be a positive integer")?;
        Ok(match action {
            "wait" => Request::Wait(token),
            "cancel" => Request::Cancel(token),
            _ => {
                let path = text("path")?;
                if path.is_empty() || path.len() > 4096 {
                    return Err("World probe path must contain 1 to 4096 bytes".into());
                }
                Request::Save(token, path.to_owned())
            }
        })
    }
}
#[derive(Clone)]
struct Witness {
    frame: u64,
    preview: Option<SharedStroke>,
}
struct Flight {
    slot: CaptureSlot,
    witness: Witness,
    epoch: u64,
}
#[derive(PartialEq)]
enum Phase {
    Baseline,
    Armed,
    Matched,
}
struct Probe {
    token: u64,
    spec: Spec,
    subject: Subject,
    geometry: CaptureGeometry,
    epoch: u64,
    deadline: Instant,
    phase: Phase,
    passes: u32,
    issued: u32,
    arm_reply: Option<Reply>,
    waiter: Option<Reply>,
    rendered: Option<Witness>,
    flight: Option<Flight>,
    evidence: Option<Value>,
    image: Option<Arc<ColorImage>>,
}
impl Drop for Probe {
    fn drop(&mut self) {
        if let Some(flight) = self.flight.take() {
            flight.slot.cancel();
        }
        let error = json!({"ok":false,"error":"World pixel probe cancelled"});
        for reply in [self.arm_reply.take(), self.waiter.take()].into_iter().flatten() {
            let _ = reply.send(error.clone());
        }
    }
}
#[derive(Default)]
pub(crate) struct State {
    probe: Option<Box<Probe>>,
    geometry: Option<CaptureGeometry>,
    epoch: u64,
}
impl State {
    pub(crate) fn active(&self) -> bool {
        self.probe.is_some()
    }
    fn needs_paint(&self) -> bool {
        self.probe.as_ref().is_some_and(|probe| probe.phase != Phase::Matched && probe.flight.is_none())
    }
    pub(crate) fn begin_pass(&mut self) {
        if let Some(probe) = self.probe.as_mut() {
            probe.rendered = None;
        }
    }
    pub(crate) fn cancel(&mut self, reason: &str) {
        if let Some(mut probe) = self.probe.take() {
            let error = json!({"ok":false,"error":format!("World pixel probe: {reason}")});
            for reply in [probe.arm_reply.take(), probe.waiter.take()].into_iter().flatten() {
                let _ = reply.send(error.clone());
            }
        }
    }
    fn observe(&mut self, subject: Option<Subject>, geometry: CaptureGeometry, now: Instant) {
        if self.geometry != Some(geometry) {
            self.epoch = self.epoch.saturating_add(1);
            self.geometry = Some(geometry);
        }
        let reason = self.probe.as_ref().and_then(|probe| {
            if now >= probe.deadline {
                Some("four-second deadline expired")
            } else if subject.as_ref() != Some(&probe.subject) {
                Some("canvas, account, grant, activation, camera or revision changed")
            } else if geometry.minimized || geometry != probe.geometry || self.epoch != probe.epoch {
                Some("window geometry changed")
            } else {
                None
            }
        });
        if let Some(reason) = reason {
            self.cancel(reason);
        }
    }
    pub(crate) fn obscured(&mut self, rect: Rect) {
        if let Some(probe) = self.probe.as_mut() {
            let radius = 3.0 / probe.geometry.pixels_per_point;
            let point = egui::pos2(probe.spec.point[0] as f32, probe.spec.point[1] as f32);
            if rect.intersects(Rect::from_center_size(point, egui::vec2(radius * 2.0, radius * 2.0))) {
                probe.rendered = None;
            }
        }
    }
}
fn ok(value: Value) -> Outcome {
    Outcome::Done(json!({"ok":true,"result":value}))
}
fn error(message: impl ToString) -> Outcome {
    Outcome::Done(json!({"ok":false,"error":message.to_string()}))
}

pub(crate) fn control(app: &mut PhotocraftApp, ctx: &Context, value: &Value, reply: Reply) -> Outcome {
    let request = match parse(value) {
        Ok(request) => request,
        Err(e) => return error(e),
    };
    observe(app, ctx);
    match request {
        Request::Arm(mut spec) => {
            if app.world_probe.active() || app.captures.any_pending() {
                return error("Finish or cancel the outstanding capture before arming a world probe");
            }
            let Some(subject) = Subject::current(app) else { return error("Open an authenticated live shared canvas before arming a probe") };
            if subject.canvas != spec.canvas || subject.account.profile == spec.sender {
                return error("World probe canvas or remote sender differs from the active subject");
            }
            let geometry = CaptureGeometry::current(ctx, None);
            if geometry.minimized || geometry.expected_bytes().is_none_or(|bytes| bytes > MAX_IMAGE_BYTES) {
                return error("World probe geometry exceeds its capture bound");
            }
            let [x, y] = spec.point;
            if x < 3.0 || y < 3.0 || x + 3.0 >= f64::from(geometry.logical_size.x) || y + 3.0 >= f64::from(geometry.logical_size.y) {
                return error("World probe patch lies outside the window");
            }
            for peer in &app.session.worlds.peers {
                if peer.profile == spec.sender
                    && let Some(stroke) = &peer.stroke
                {
                    spec.excluded.insert(stroke.id);
                }
            }
            if spec.excluded.len() > MAX_EXCLUDED {
                return error("World probe has too many prior strokes");
            }
            app.world_probe.observe(Some(subject.clone()), geometry, Instant::now());
            let Some(next_id) = app.ui.next_id.checked_add(1) else { return error("World probe token space exhausted") };
            let token = app.ui.next_id;
            app.ui.next_id = next_id;
            app.world_probe.probe = Some(Box::new(Probe {
                token,
                spec,
                subject,
                geometry,
                epoch: app.world_probe.epoch,
                deadline: Instant::now() + DEADLINE,
                phase: Phase::Baseline,
                passes: 0,
                issued: 0,
                arm_reply: Some(reply),
                waiter: None,
                rendered: None,
                flight: None,
                evidence: None,
                image: None,
            }));
            ctx.request_repaint();
            ctx.request_repaint_after(DEADLINE);
            Outcome::Deferred
        }
        Request::Wait(token) => {
            let Some(probe) = app.world_probe.probe.as_mut().filter(|probe| probe.token == token) else { return error("Unknown or expired world probe token") };
            if probe.phase == Phase::Baseline {
                return error("Wait for the world probe arm acknowledgement");
            }
            if let Some(evidence) = &probe.evidence {
                return ok(evidence.clone());
            }
            if probe.waiter.is_some() {
                return error("World probe already has a waiter");
            }
            probe.waiter = Some(reply);
            Outcome::Deferred
        }
        Request::Cancel(token) => {
            if app.world_probe.probe.as_ref().is_none_or(|probe| probe.token != token) {
                return error("Unknown or expired world probe token");
            }
            app.world_probe.cancel("cancelled by caller");
            ok(json!({"token":token,"cancelled":true}))
        }
        Request::Save(token, path) => {
            let Some(probe) = app.world_probe.probe.as_ref().filter(|probe| probe.token == token) else { return error("Unknown or expired world probe token") };
            let Some(image) = probe.image.clone() else { return error("World probe has no matched image") };
            let [width, height] = image.size;
            let Some(bytes) = width.checked_mul(height).and_then(|count| count.checked_mul(4)).filter(|bytes| *bytes <= MAX_IMAGE_BYTES) else {
                return error("World probe image exceeds its byte bound");
            };
            let mut rgba = Vec::with_capacity(bytes);
            rgba.extend(image.pixels.iter().flat_map(|pixel| pixel.to_array()));
            let result = crate::control::save_screenshot_rgba(app, width, height, &rgba, Some(&path));
            if result.get("ok").and_then(Value::as_bool) == Some(true) {
                app.world_probe.cancel("saved");
            }
            Outcome::Done(result)
        }
    }
}

pub(crate) fn observe(app: &mut PhotocraftApp, ctx: &Context) {
    if !app.world_probe.active() {
        return;
    }
    let subject = Subject::current(app);
    app.world_probe.observe(subject, CaptureGeometry::current(ctx, None), Instant::now());
}

// Called only after render_available succeeded with this exact preview list.
pub(crate) fn rendered(app: &mut PhotocraftApp, ui: &egui::Ui, rect: Rect, view: &Viewport, previews: &[SharedStroke]) {
    let Some(probe) = app.world_probe.probe.as_mut() else { return };
    probe.rendered = None;
    if view.camera != probe.subject.camera || !rect.contains(egui::pos2(probe.spec.point[0] as f32, probe.spec.point[1] as f32)) {
        return;
    }
    // A dark target must not be attributable to a different transient stroke.
    if previews.iter().any(|preview| {
        let stroke = &preview.stroke;
        stroke.points.len() > 128
            || ((stroke.owner != probe.spec.sender || probe.spec.excluded.contains(&stroke.id))
                && covers(stroke, view, rect, probe.spec.point, ui.ctx().pixels_per_point()))
    }) {
        return;
    }
    let mut candidates = previews.iter().filter(|preview| {
        let stroke = &preview.stroke;
        stroke.owner == probe.spec.sender
            && stroke.canvas == probe.subject.canvas
            && !probe.spec.excluded.contains(&stroke.id)
            && (2..=128).contains(&stroke.points.len())
            && !stroke.erase
            && intersects(stroke, view, rect, probe.spec.point, ui.ctx().pixels_per_point())
    });
    let candidate = candidates.next().cloned();
    if candidates.next().is_some() {
        return;
    }
    if let Some(candidate) = &candidate
        && previews
            .iter()
            .any(|other| other.stroke.id != candidate.stroke.id && covers(&other.stroke, view, rect, probe.spec.point, ui.ctx().pixels_per_point()))
    {
        return;
    }
    probe.rendered = Some(Witness { frame: app.frame, preview: candidate });
}
fn intersects(stroke: &scene::Stroke, view: &Viewport, rect: Rect, point: [f64; 2], density: f32) -> bool {
    near_stroke(stroke, view, rect, point, 2.0 / f64::from(density))
}
fn covers(stroke: &scene::Stroke, view: &Viewport, rect: Rect, point: [f64; 2], density: f32) -> bool {
    let Ok(ratio) = scene::coordinates::scale_ratio(stroke.anchor.depth, &view.camera) else { return true };
    let radius = stroke.width * ratio * 0.5 + 4.0 / f64::from(density);
    !radius.is_finite() || near_stroke(stroke, view, rect, point, radius)
}
fn near_stroke(stroke: &scene::Stroke, view: &Viewport, rect: Rect, point: [f64; 2], radius: f64) -> bool {
    let target = [point[0] - f64::from(rect.left()), point[1] - f64::from(rect.top())];
    let project = |local| scene::coordinates::point(&stroke.anchor, local).and_then(|p| scene::coordinates::point_to_screen(view, p));
    if stroke.points.len() == 1
        && let Some(point) = stroke.points.first()
    {
        return project(*point).is_ok_and(|point| (target[0] - point[0]).hypot(target[1] - point[1]) <= radius);
    }
    stroke.points.windows(2).any(|pair| {
        let (Some(a), Some(b)) = (pair.first(), pair.get(1)) else { return false };
        let (Ok(a), Ok(b)) = (project(*a), project(*b)) else { return false };
        let delta = [b[0] - a[0], b[1] - a[1]];
        let length = delta[0] * delta[0] + delta[1] * delta[1];
        if !length.is_finite() {
            return false;
        }
        if length <= 0.0 {
            return (target[0] - a[0]).hypot(target[1] - a[1]) <= radius;
        }
        let along = (((target[0] - a[0]) * delta[0] + (target[1] - a[1]) * delta[1]) / length).clamp(0.0, 1.0);
        let dx = target[0] - a[0] - along * delta[0];
        let dy = target[1] - a[1] - along * delta[1];
        dx * dx + dy * dy <= radius * radius
    })
}

pub(crate) fn issue(app: &mut PhotocraftApp, ctx: &Context) {
    observe(app, ctx);
    if ctx.will_discard() {
        return;
    }
    let Some(probe) = app.world_probe.probe.as_mut() else { return };
    if probe.phase == Phase::Matched || probe.flight.is_some() {
        return;
    }
    probe.passes = probe.passes.saturating_add(1);
    if probe.passes < crate::screenshot::SETTLE_PASSES {
        return;
    }
    let Some(witness) = probe.rendered.as_ref().filter(|witness| witness.frame == app.frame) else { return };
    if probe.phase == Phase::Armed && witness.preview.is_none() {
        return;
    }
    if probe.issued >= MAX_CAPTURES {
        app.world_probe.cancel("capture count exceeded");
        return;
    }
    let (slot, callback) = CaptureSlot::new(ctx, probe.geometry.viewport, MAX_IMAGE_BYTES);
    probe.flight = Some(Flight { slot, witness: witness.clone(), epoch: probe.epoch });
    probe.issued += 1;
    crate::screenshot::send_command(ctx, callback);
}

fn pixels(image: &ColorImage, geometry: &CaptureGeometry, point: [f64; 2]) -> Result<PixelSample, String> {
    if geometry.expected_pixels() != Some(image.size) {
        return Err("World probe image dimensions differ from its issued geometry".into());
    }
    let [width, height] = image.size;
    if width.checked_mul(height) != Some(image.pixels.len()) {
        return Err("World probe image is malformed".into());
    }
    let x = (point[0] * width as f64 / f64::from(geometry.logical_size.x)).round() as usize;
    let y = (point[1] * height as f64 / f64::from(geometry.logical_size.y)).round() as usize;
    if x < 2 || y < 2 || x.checked_add(2).is_none_or(|n| n >= width) || y.checked_add(2).is_none_or(|n| n >= height) {
        return Err("World probe pixel patch lies outside its image".into());
    }
    let mut result = Vec::with_capacity(25);
    let mut dark = 0;
    for row in y - 2..=y + 2 {
        for column in x - 2..=x + 2 {
            let index = row.checked_mul(width).and_then(|n| n.checked_add(column)).ok_or("World probe pixel offset overflow")?;
            let pixel = image.pixels.get(index).ok_or("World probe pixel is unavailable")?.to_array();
            if pixel[0] < 100 && pixel[1] < 100 && pixel[2] < 100 {
                dark += 1;
            }
            result.push(pixel);
        }
    }
    Ok(([x, y], result, dark))
}
fn evidence(probe: &Probe, flight: &Flight, image: &ColorImage, pixel: [usize; 2], rgba: Vec<[u8; 4]>, dark: usize) -> Value {
    let preview = flight.witness.preview.as_ref().map(|shared| {
        let stroke = &shared.stroke;
        json!({"id":stroke.id,"owner":stroke.owner,"device":shared.device,"anchor":stroke.anchor,"points":stroke.points,
            "width":stroke.width,"color":stroke.color,"erase":stroke.erase})
    });
    json!({"token":probe.token,"matched":probe.phase == Phase::Armed && dark >= 3 && preview.is_some(),"frame":flight.witness.frame,"pixel":pixel,"imageSize":image.size,
        "pixels":rgba,"darkPixels":dark,"geometryEpoch":flight.epoch,"scope":probe.subject.json(),"preview":preview})
}
pub(crate) fn drain(app: &mut PhotocraftApp, ctx: &Context) {
    observe(app, ctx);
    // Match ordinary unissued captures: a hidden host otherwise runs logic
    // without a UI pass. Existing repaint timers still control the cadence.
    if app.world_probe.needs_paint() {
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
    }
    let Some(probe) = app.world_probe.probe.as_mut() else { return };
    let Some(flight) = probe.flight.take() else { return };
    let Some(result) = flight.slot.take() else {
        probe.flight = Some(flight);
        return;
    };
    let image = match result {
        Ok(image) => image,
        Err(e) => {
            app.world_probe.cancel(&e.to_string());
            return;
        }
    };
    let (pixel, rgba, dark) = match pixels(&image, &probe.geometry, probe.spec.point) {
        Ok(result) => result,
        Err(e) => {
            app.world_probe.cancel(&e);
            return;
        }
    };
    let result = evidence(probe, &flight, &image, pixel, rgba, dark);
    if probe.phase == Phase::Baseline {
        if dark >= 3 {
            app.world_probe.cancel("baseline target was not blank");
            return;
        }
        probe.phase = Phase::Armed;
        if let Some(reply) = probe.arm_reply.take() {
            let _ = reply.send(json!({"ok":true,"result":{"token":probe.token,"armed":true,"baseline":result}}));
        }
    } else if dark >= 3 && flight.witness.preview.is_some() {
        probe.phase = Phase::Matched;
        probe.image = Some(image);
        probe.evidence = Some(result.clone());
        if let Some(reply) = probe.waiter.take() {
            let _ = reply.send(json!({"ok":true,"result":result}));
        }
    }
}

#[cfg(test)]
mod tests;
