use super::{CommandSpec, Deserialize, RegionKey, StrokeId, active, edit, error, fresh, identity, json, opened, parse, spec};
use photocraft_scene::shared::{StrokeCopyTarget, media::Credit};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Copy {
    stroke: StrokeId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Paste {
    id: Option<StrokeId>,
    anchor: RegionKey,
    offset: [f64; 2],
    scale: f64,
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        spec("komaphone.world.stroke.copy", "Copy a synced stroke", r#"{"stroke":stroke_id}"#, opened, |s, p| {
            let p: Copy = parse(p)?;
            let active = active(s)?;
            let source =
                active.canonical.strokes.get(&p.stroke).filter(|source| source.visible).ok_or_else(|| error("Choose a visible, synchronized stroke"))?;
            let credit = if let Some(provenance) = &source.provenance {
                provenance.author.clone()
            } else if let Some(credit) = active.canonical.imported_credit(source.stroke.owner) {
                credit
            } else {
                let person = s
                    .worlds
                    .people
                    .iter()
                    .find(|person| person.profile == source.stroke.owner)
                    .ok_or_else(|| error("Refresh people before copying this artist's stroke"))?;
                Credit { profile: person.profile, label: person.display_name.clone() }
            };
            s.worlds.stroke_copy_source = Some((std::sync::Arc::clone(&active.canonical), p.stroke, credit));
            Ok(json!({"copied":true,"stroke":p.stroke}))
        }),
        spec(
            "komaphone.world.stroke.paste",
            "Paste an independent stroke into this canvas",
            r#"{"id":new_stroke_id?,"anchor":region,"offset":[x,y],"scale":number}"#,
            opened,
            |s, p| {
                let p: Paste = parse(p)?;
                let (world, source, credit) = s.worlds.stroke_copy_source.as_ref().ok_or_else(|| error("Copy a synchronized stroke first"))?;
                let target = StrokeCopyTarget {
                    canvas: active(s)?.info.canvas,
                    id: p.id.map_or_else(|| StrokeId::new(&fresh()?).map_err(error), Ok)?,
                    owner: identity(s)?.0,
                    anchor: p.anchor,
                    offset: p.offset,
                    scale: p.scale,
                };
                let action = world.copy_stroke(*source, credit.clone(), target).map_err(error)?;
                edit(s, action)
            },
        ),
    ]
}
