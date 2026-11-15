use super::{
    CommandSpec, Deserialize, Edit, OwnerId, Result, Session, Value, active, edit, empty, error, fresh, identity, json, navigation, opened, parse, spec,
};
use photocraft_scene::shared::tours::{TourPlayback, TourStop};
use photocraft_scene::shared::{Tour, TourId};
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutTour {
    id: Option<TourId>,
    name: String,
    stops: Vec<TourStop>,
    expected_revision: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: TourId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TourRevision {
    id: TourId,
    expected_revision: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TourControl {
    state: String,
    elapsed_ms: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewControl {
    state: String,
    speed: Option<f64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scrub {
    revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Changes {
    after: u64,
    limit: Option<usize>,
    authors: Option<Vec<OwnerId>>,
    #[serde(default)]
    include_own: bool,
}

fn tour(s: &Session, id: TourId) -> Result<&Tour> {
    active(s)?.display.tours.get(&id).ok_or_else(|| error("Tour was not found"))
}
fn review(s: &mut Session, change: impl FnOnce(&mut photocraft_scene::shared::ReviewState, u64) -> Result<()>) -> Result<Value> {
    let mut viewer = active(s)?.viewer.clone();
    let revision = active(s)?.canonical.revision;
    let state = viewer.review.as_mut().ok_or_else(|| error("This canvas's first-visit baseline is still loading"))?;
    if state.viewer != identity(s)?.0 || state.canvas != active(s)?.info.canvas {
        return Err(error("Review belongs to another viewer or canvas"));
    }
    change(state, revision)?;
    state.validate(revision).map_err(error)?;
    navigation::save(s, viewer)
}
pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        spec(
            "komaphone.world.tour.put",
            "Save shared camera tour",
            r#"{"id":tour_id?,"name":string,"stops":[{"camera":camera,"travel_ms":integer,"pause_ms":integer,"target":stroke_id?}],"expected_revision":integer?}"#,
            opened,
            |s, p| {
                if p.get("stops").and_then(Value::as_array).is_none_or(|stops| stops.is_empty() || stops.len() > 128) {
                    return Err(error("A tour needs 1–128 stops"));
                }
                let p: PutTour = parse(p)?;
                let id = match p.id {
                    Some(id) => id,
                    None => TourId::new(&fresh()?).map_err(error)?,
                };
                let expected_revision = p.expected_revision.unwrap_or(active(s)?.display.tour_revision(id));
                let tour = Tour { id, canvas: active(s)?.info.canvas, name: p.name, revision: 0, stops: p.stops };
                tour.validate().map_err(error)?;
                edit(s, Edit::PutTour { tour, expected_revision })
            },
        ),
        spec("komaphone.world.tour.remove", "Remove shared camera tour", r#"{"id":tour_id,"expected_revision":integer?}"#, opened, |s, p| {
            let p: TourRevision = parse(p)?;
            let expected_revision = p.expected_revision.unwrap_or(tour(s, p.id)?.revision);
            edit(s, Edit::RemoveTour { tour: p.id, expected_revision })
        }),
        spec(
            "komaphone.world.tour.skipMissing",
            "Explicitly skip missing tour destinations",
            r#"{"id":tour_id,"expected_revision":integer?}"#,
            opened,
            |s, p| {
                let p: TourRevision = parse(p)?;
                let old = tour(s, p.id)?;
                let expected_revision = p.expected_revision.unwrap_or(old.revision);
                let tour = old.skipping_missing(&active(s)?.display).map_err(error)?;
                edit(s, Edit::PutTour { tour, expected_revision })
            },
        ),
        spec("komaphone.world.tour.play", "Play shared camera tour", r#"{"id":tour_id}"#, opened, |s, p| {
            let p: Id = parse(p)?;
            let tour = tour(s, p.id)?;
            if !tour.missing_stops(&active(s)?.display).map_err(error)?.is_empty() {
                return Err(error("Repair this tour or explicitly skip its missing destinations"));
            }
            let playback = TourPlayback::new(tour).map_err(error)?;
            let frame = tour.frame_at(0).map_err(error)?;
            let mut viewer = active(s)?.viewer.clone();
            viewer.camera = frame.camera;
            let result = navigation::save(s, viewer)?;
            s.worlds.tour_playback = Some(playback);
            Ok(result)
        }),
        spec("komaphone.world.tour.control", "Control camera tour", r#"{"state":"pause|resume|cancel","elapsed_ms":integer?}"#, opened, |s, p| {
            let p: TourControl = parse(p)?;
            if !["pause", "resume", "cancel"].contains(&p.state.as_str()) {
                return Err(error("Tour state must be pause, resume or cancel"));
            }
            let mut playback = s.worlds.tour_playback.clone().ok_or_else(|| error("No tour is playing"))?;
            if p.state == "cancel" {
                if p.elapsed_ms.is_some() {
                    return Err(error("Cancel does not take a scrub position"));
                }
                s.worlds.tour_playback = None;
                return Ok(json!({"cancelled":true}));
            }
            let tour = tour(s, playback.tour)?;
            playback.validate(tour).map_err(error)?;
            let mut viewer = active(s)?.viewer.clone();
            if let Some(elapsed) = p.elapsed_ms {
                viewer.camera = playback.scrub(tour, elapsed).map_err(error)?.camera;
            }
            playback.playing = p.state == "resume";
            let result = navigation::save(s, viewer)?;
            s.worlds.tour_playback = Some(playback);
            Ok(result)
        }),
        spec("komaphone.world.review.begin", "Review unseen canvas changes", "{}", opened, |s, p| {
            empty(p)?;
            review(s, |state, revision| state.begin(revision).map_err(error))
        }),
        spec("komaphone.world.review.control", "Control change playback", r#"{"state":"play|pause|cancel","speed":number=1(0.25..8)}"#, opened, |s, p| {
            let p: ReviewControl = parse(p)?;
            if !["play", "pause", "cancel"].contains(&p.state.as_str()) || p.speed.is_some_and(|n| !n.is_finite() || !(0.25..=8.).contains(&n)) {
                return Err(error("Invalid review playback state or speed"));
            }
            review(s, |state, _| {
                if p.state == "cancel" {
                    if p.speed.is_some() {
                        return Err(error("Cancel does not take a playback speed"));
                    }
                    state.cancel();
                    return Ok(());
                }
                let interval = state.interval.as_mut().ok_or_else(|| error("Begin a review first"))?;
                interval.playing = p.state == "play";
                if let Some(speed) = p.speed {
                    interval.speed = speed;
                }
                Ok(())
            })
        }),
        spec("komaphone.world.review.scrub", "Scrub change playback", r#"{"revision":integer}"#, opened, |s, p| {
            let p: Scrub = parse(p)?;
            review(s, |state, _| state.scrub(p.revision).map_err(error))
        }),
        spec("komaphone.world.review.markSeen", "Mark the fixed review interval seen", "{}", opened, |s, p| {
            empty(p)?;
            review(s, |state, revision| state.mark_seen(revision).map_err(error))
        }),
        spec(
            "komaphone.world.review.changes",
            "Inspect review highlights",
            r#"{"after":revision,"limit":integer=128(max256),"authors":[profile_id]?,"include_own":false}"#,
            opened,
            |s, p| {
                if p.get("authors").and_then(Value::as_array).is_some_and(|v| v.len() > 128) {
                    return Err(error("Review highlight filter has too many authors"));
                }
                let p: Changes = parse(p)?;
                let authors = p.authors.map(|v| v.into_iter().collect::<BTreeSet<_>>());
                let active = active(s)?;
                let review = active.viewer.review.as_ref().ok_or_else(|| error("Begin a review first"))?;
                let changes = review.changes(&active.canonical, p.after, p.limit.unwrap_or(128), authors.as_ref(), p.include_own).map_err(error)?;
                serde_json::to_value(changes).map_err(error)
            },
        ),
    ]
}
