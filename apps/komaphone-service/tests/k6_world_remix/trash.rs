use super::*;

#[test]
fn deleted_media_source_and_destination_deny_assets_remix_and_archive_copy_until_owner_restores() {
    let mut f = Fixture::new(Mode::Animation { rotations: 1 }, 2);
    f.complete();
    invite(&mut f.store, 1, 2, 100, 7000);
    let source = f.source(None);
    let media = prepared(&mut f.store, 1, 100, source.clone());
    let original = operation(1, 100, 1, 8000, &media);
    world(&mut f.store, 1, Action::RemixGame { operation: original.clone(), room: source.room });
    let bytes = fetch(&mut f.store, 2, 100, media.frames[0].hash);
    let accepted = events(&mut f.store, 1, 100);
    world(&mut f.store, 1, Action::Delete { canvas: canvas(100), expected_epoch: 1 });
    for actor in [1, 2, 3] {
        denied(&mut f.store, actor, Action::FetchMedia { canvas: canvas(100), hash: media.frames[0].hash, offset: 0 });
        denied(&mut f.store, actor, Action::PrepareGameMedia { canvas: canvas(100), source: source.clone() });
        denied(&mut f.store, actor, Action::RemixGame { operation: original.clone(), room: source.room });
        denied(
            &mut f.store,
            actor,
            Action::RestoreCopy { source_canvas: canvas(100), source_revision: 1, source_hash: media.provenance.artifact_hash, new_canvas: canvas(200) },
        );
    }
    create(&mut f.store, 1, 200);
    let mut copied = media.clone();
    copied.provenance.source = MediaSource::Canvas { canvas: canvas(100), media: MediaId::new(&hex(8000)).unwrap() };
    copied.provenance.source_revision = 1;
    let copied = operation(1, 200, 1, 8001, &copied);
    denied(&mut f.store, 1, Action::RemixCanvas { operation: copied.clone(), source_canvas: canvas(100), source_media: MediaId::new(&hex(8000)).unwrap() });
    world(&mut f.store, 1, Action::RestoreOriginal { canvas: canvas(100), expected_epoch: 2 });
    assert_eq!(fetch(&mut f.store, 2, 100, media.frames[0].hash), bytes);
    assert_eq!(events(&mut f.store, 2, 100), accepted);
    denied(&mut f.store, 1, Action::RemixGame { operation: original, room: source.room }); // Original receipt has a revoked epoch.
    world(&mut f.store, 1, Action::RemixCanvas { operation: copied, source_canvas: canvas(100), source_media: MediaId::new(&hex(8000)).unwrap() });
    world(&mut f.store, 1, Action::Delete { canvas: canvas(200), expected_epoch: 1 });
    denied(&mut f.store, 1, Action::FetchMedia { canvas: canvas(200), hash: media.frames[0].hash, offset: 0 });
    assert_eq!(fetch(&mut f.store, 2, 100, media.frames[0].hash), bytes);
}
