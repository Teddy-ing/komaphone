//! World-space `260×260` LOD tile pyramid, octave-bucketed spatial index with
//! bounded sparse micro-summaries, multi-band dynamic composition, lazy per-tile
//! dirty-rect validation, cooperative-cancel worker pool, and optional disk cache.
pub mod compose;
pub mod index;
pub mod manager;

pub use compose::{DynamicSplitCache, RgbaFrame, TileComposeParams, TileComposeResult, compose_world_tile};
pub use index::{MAX_INTERLEAVED_EXPAND, MAX_SUMMARY_LEVEL, MAX_SUMMARY_NODES_PER_TILE, SceneObject, SummaryEpoch, SummaryKey, TileQuery, WorldSceneIndex};
pub use manager::{CachedCpuTile, CachedDynamicTile, DirtyLog, ScopeKey, SplitWarmJob, TileBudget, TileManagerStats, WorldTileManager};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use photocraft_color::BlendMode;
    use photocraft_paint::presets;
    use photocraft_scene::shared::media::{AssetHash, Credit, MediaFrame, MediaSource, Provenance};
    use photocraft_scene::shared::{AcceptedOp, DeviceId, Edit, MediaId, MediaPlacement, OpId, SharedStroke, SourceId, WorldOp, WorldState};
    use photocraft_scene::spatial::{TILE_APRON, TILE_LOGICAL, TILE_PHYSICAL, TileKey, WorldRect, select_target_lod};
    use photocraft_scene::{CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId, Viewport, coordinates};

    use super::*;
    use crate::world_brush::{LiveTileBrush, to_stroke_brush};

    fn hex(n: u128) -> String {
        format!("{n:032x}")
    }
    fn cid() -> CanvasId {
        CanvasId::new(&hex(1)).unwrap()
    }
    fn owner(n: u128) -> OwnerId {
        OwnerId::new(&hex(10 + n)).unwrap()
    }
    fn dev(n: u128) -> DeviceId {
        DeviceId::new(&hex(100 + n)).unwrap()
    }

    fn append_stroke(world: &mut WorldState, writer: u128, stroke: Stroke) {
        let rev = world.revision + 1;
        let seq = world.next_sequence(dev(writer)).unwrap();
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(10_000 + u128::from(rev))).unwrap(),
                    canvas: world.canvas,
                    author: owner(writer),
                    device: dev(writer),
                    sequence: seq,
                    authority_epoch: 1,
                    action: Edit::AddStroke { stroke },
                },
            })
            .unwrap();
    }

    fn make_brush_stroke(
        id: u128,
        writer: u128,
        depth: u8,
        name: &str,
        pts: Vec<[f64; 2]>,
        tweak: impl FnOnce(&mut photocraft_paint::BrushSettings),
    ) -> Stroke {
        let all = presets::builtin();
        let mut b = presets::find(&all, name).unwrap().brush.clone();
        tweak(&mut b);
        let sb = to_stroke_brush(&b, 1.0, false).unwrap();
        Stroke {
            id: StrokeId::new(&hex(id)).unwrap(),
            canvas: cid(),
            owner: owner(writer),
            revision: 0,
            anchor: RegionKey { depth, ..Default::default() },
            points: pts,
            width: f64::from(b.size),
            color: Srgba { rgba: b.color },
            erase: b.erase,
            brush: Some(Box::new(sb)),
            samples: None,
        }
    }

    #[test]
    fn adjacent_tiles_match_bit_identically_across_negative_axis_4way_seams() {
        let mut world = WorldState::new(cid(), owner(0), "Seams".into()).unwrap();
        // Crossing (0, 0) spans 4 tiles: (-1,-1), (0,-1), (-1,0), (0,0) with soft, wet, texture, Dissolve, and Multiply.
        append_stroke(
            &mut world,
            0,
            make_brush_stroke(1, 0, 0, "Watercolor Wet Edges", vec![[-60.0, -40.0], [60.0, 40.0]], |b| {
                b.size = 48.0;
                b.color = [0.15, 0.45, 0.85, 1.0];
                b.seed = 42;
            }),
        );
        append_stroke(
            &mut world,
            0,
            make_brush_stroke(2, 0, 0, "Chalk", vec![[-50.0, 30.0], [50.0, -30.0]], |b| {
                b.size = 36.0;
                b.mode = BlendMode::Multiply;
                b.color = [0.85, 0.25, 0.20, 1.0];
                b.seed = 77;
            }),
        );
        append_stroke(
            &mut world,
            1,
            make_brush_stroke(3, 1, 0, "Soft Round", vec![[-40.0, 0.0], [40.0, 0.0]], |b| {
                b.size = 32.0;
                b.mode = BlendMode::Dissolve;
                b.opacity = 0.6;
                b.color = [0.10, 0.70, 0.20, 1.0];
                b.seed = 99;
            }),
        );

        let index = WorldSceneIndex::new(&world).unwrap();
        let view = Viewport { camera: coordinates::home(cid()), width: 256, height: 256 };
        let lod = select_target_lod(&view, 1.0, 144, None).unwrap();
        let compose = |tx: i128, ty: i128| {
            let t = TileKey::new(lod, tx, ty).unwrap();
            let params = TileComposeParams {
                index: &index,
                tile: t,
                scope_gen: 1,
                validated_seq: 1,
                elapsed_ms: 0,
                previews: &[],
                live_brush: None,
                allow_parallel_brush: false,
                cancel: None,
            };
            compose_world_tile(&params, None, &mut |_| Ok(None)).unwrap().rgba8
        };
        let tl = compose(-1, -1);
        let tr = compose(0, -1);
        let bl = compose(-1, 0);
        let br = compose(0, 0);

        let side = TILE_PHYSICAL as usize;
        let apron = (2 * TILE_APRON) as usize; // 4 overlapping columns/rows: [256..260] vs [0..4]
        let log = TILE_LOGICAL as usize;
        fn px(buf: &[u8], x: usize, y: usize) -> &[u8] {
            let side = TILE_PHYSICAL as usize;
            &buf[(y * side + x) * 4..(y * side + x) * 4 + 4]
        }

        // Vertical seam between (-1, 0) and (0, 0) across all 260 rows & 4 overlapping apron columns.
        for y in 0..side {
            for k in 0..apron {
                assert_eq!(px(&bl, log + k, y), px(&br, k, y), "V seam mismatch at row {y} col_off {k}");
                assert_eq!(px(&tl, log + k, y), px(&tr, k, y), "Top V seam mismatch at row {y} col_off {k}");
            }
        }
        // Horizontal seam between (0, -1) and (0, 0) across all 260 cols & 4 overlapping apron rows.
        for x in 0..side {
            for k in 0..apron {
                assert_eq!(px(&tr, x, log + k), px(&br, x, k), "H seam mismatch at col {x} row_off {k}");
                assert_eq!(px(&tl, x, log + k), px(&bl, x, k), "Left H seam mismatch at col {x} row_off {k}");
            }
        }
        // 4-way corner overlap across (-1,-1), (0,-1), (-1,0), (0,0).
        for ky in 0..apron {
            for kx in 0..apron {
                let c = px(&tl, log + kx, log + ky);
                assert_eq!(c, px(&tr, kx, log + ky));
                assert_eq!(c, px(&bl, log + kx, ky));
                assert_eq!(c, px(&br, kx, ky));
            }
        }
    }

    #[test]
    fn cold_overview_includes_tiny_marks_exceeds_4096_and_preserves_interleaved_w4_order() {
        let mut world = WorldState::new(cid(), owner(0), "Overview + Interleaved".into()).unwrap();
        // Device 0 underneath: coarse yellow background stroke covering [0..100, 0..100].
        append_stroke(
            &mut world,
            0,
            Stroke {
                id: StrokeId::new(&hex(1)).unwrap(),
                canvas: cid(),
                owner: owner(0),
                revision: 0,
                anchor: RegionKey { depth: 0, ..Default::default() },
                points: vec![[32.0, 32.0], [64.0, 32.0]],
                width: 48.0,
                color: Srgba { rgba: [1.0, 1.0, 0.0, 1.0] },
                erase: false,
                brush: None,
                samples: None,
            },
        );
        // Device 1, rev 2: tiny sub-pixel red marks (depth 1, 0.4 px wide at depth 0) at (32, 32) — BEFORE coarse erase!
        for k in 0..8u128 {
            let x = 3200.0 + (k as f64) * 5.0;
            append_stroke(
                &mut world,
                1,
                Stroke {
                    id: StrokeId::new(&hex(100 + k)).unwrap(),
                    canvas: cid(),
                    owner: owner(1),
                    revision: 0,
                    anchor: RegionKey { depth: 1, ..Default::default() },
                    points: vec![[x, 3200.0], [x + 4.0, 3200.0]],
                    width: 4.0,
                    color: Srgba { rgba: [1.0, 0.0, 0.0, 1.0] },
                    erase: false,
                    brush: None,
                    samples: None,
                },
            );
        }
        // Device 1, rev 10: coarse depth-0 eraser over (32, 32) — MUST erase rev 2..9 red marks on dev 1 ONLY!
        append_stroke(
            &mut world,
            1,
            Stroke {
                id: StrokeId::new(&hex(200)).unwrap(),
                canvas: cid(),
                owner: owner(1),
                revision: 0,
                anchor: RegionKey { depth: 0, ..Default::default() },
                points: vec![[32.0, 32.0]],
                width: 48.0,
                color: Srgba { rgba: [0.0, 0.0, 0.0, 1.0] },
                erase: true,
                brush: None,
                samples: None,
            },
        );
        // Device 1, rev 11..18: tiny sub-pixel blue marks at (32, 32) — AFTER coarse erase, MUST survive!
        for k in 0..8u128 {
            let x = 3200.0 + (k as f64) * 5.0;
            append_stroke(
                &mut world,
                1,
                Stroke {
                    id: StrokeId::new(&hex(300 + k)).unwrap(),
                    canvas: cid(),
                    owner: owner(1),
                    revision: 0,
                    anchor: RegionKey { depth: 1, ..Default::default() },
                    points: vec![[x, 3200.0], [x + 4.0, 3200.0]],
                    width: 4.0,
                    color: Srgba { rgba: [0.0, 0.0, 1.0, 1.0] },
                    erase: false,
                    brush: None,
                    samples: None,
                },
            );
        }

        let index = WorldSceneIndex::new(&world).unwrap();
        let view = Viewport { camera: coordinates::home(cid()), width: 256, height: 256 };
        let lod = select_target_lod(&view, 1.0, 144, None).unwrap();
        let tile = TileKey::new(lod, 0, 0).unwrap();
        let q = index.query_tile(tile);
        assert!(q.summary_nodes_visited <= MAX_SUMMARY_NODES_PER_TILE);
        assert!(!q.summaries.is_empty(), "depth-1 tiny marks must appear in sub-pixel summaries at depth 0");

        let params = TileComposeParams {
            index: &index,
            tile,
            scope_gen: 1,
            validated_seq: 1,
            elapsed_ms: 0,
            previews: &[],
            live_brush: None,
            allow_parallel_brush: false,
            cancel: None,
        };
        let res = compose_world_tile(&params, None, &mut |_| Ok(None)).unwrap();
        let side = TILE_PHYSICAL as usize;
        // Sample around logical (32, 32) -> physical (34, 34). Yellow underneath (dev 0) + blue surviving on top (dev 1).
        let mut saw_blue_over_yellow = false;
        let mut saw_yellow_intact = false;
        for py in 30..=38 {
            for px in 30..=38 {
                let off = (py * side + px) * 4;
                let p = &res.rgba8[off..off + 4];
                // Yellow = [255, 255, 0]. Blue over yellow raises B > 0 and drops R/G equally (whereas red over yellow keeps B == 0!).
                if p[2] > 5 {
                    saw_blue_over_yellow = true;
                }
                if p[0] > 240 && p[1] > 240 && p[2] < 10 {
                    saw_yellow_intact = true;
                }
            }
        }
        assert!(saw_yellow_intact, "dev 0 yellow background must not be erased by dev 1 eraser");
        assert!(saw_blue_over_yellow, "post-erase sub-pixel blue marks must survive on dev 1");
    }

    #[test]
    fn dynamic_split_cache_matches_single_pass_across_live_peer_and_multi_animated_media() {
        let mut world = WorldState::new(cid(), owner(0), "Combined Dynamic".into()).unwrap();
        // Dev 0 (static bottom): opaque red horizontal bar.
        append_stroke(
            &mut world,
            0,
            make_brush_stroke(1, 0, 0, "Hard Round", vec![[20.0, 64.0], [180.0, 64.0]], |b| {
                b.size = 32.0;
                b.color = [0.9, 0.1, 0.1, 1.0];
            }),
        );
        // Dev 1 (dynamic: animated media + committed Multiply stroke + active local Soft Eraser!).
        let f1_hash = AssetHash::new(&"a".repeat(64)).unwrap();
        let f2_hash = AssetHash::new(&"b".repeat(64)).unwrap();
        let make_media = |id: u128, writer: u128, off: [f64; 2]| MediaPlacement {
            id: MediaId::new(&hex(id)).unwrap(),
            canvas: cid(),
            owner: owner(writer),
            anchor: RegionKey::default(),
            offset: off,
            width: 48.0,
            height: 48.0,
            looping: true,
            frames: vec![
                MediaFrame { hash: f1_hash, byte_len: 16, width: 2, height: 2, duration_ms: 100 },
                MediaFrame { hash: f2_hash, byte_len: 16, width: 2, height: 2, duration_ms: 100 },
            ],
            provenance: Provenance {
                source: MediaSource::Gallery { entry: SourceId::new(&hex(9)).unwrap(), room: None, game: None, chain: 0, frame: None },
                source_revision: 1,
                artifact_hash: f1_hash,
                authors: vec![Credit { profile: owner(writer), label: "Synth".into() }],
            },
        };
        let rev = world.revision + 1;
        let seq = world.next_sequence(dev(1)).unwrap();
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(8001)).unwrap(),
                    canvas: cid(),
                    author: owner(1),
                    device: dev(1),
                    sequence: seq,
                    authority_epoch: 1,
                    action: Edit::PlaceMedia { media: Box::new(make_media(901, 1, [50.0, 40.0])) },
                },
            })
            .unwrap();
        append_stroke(
            &mut world,
            1,
            make_brush_stroke(2, 1, 0, "Hard Round", vec![[50.0, 64.0], [110.0, 64.0]], |b| {
                b.size = 24.0;
                b.mode = BlendMode::Multiply;
                b.color = [0.2, 0.8, 0.9, 1.0];
            }),
        );
        // Dev 2 (static middle sandwich between dynamic Dev 1 and dynamic Dev 3!).
        append_stroke(
            &mut world,
            2,
            make_brush_stroke(3, 2, 0, "Soft Round", vec![[80.0, 20.0], [80.0, 120.0]], |b| {
                b.size = 28.0;
                b.opacity = 0.5;
                b.color = [0.1, 0.2, 0.9, 1.0];
            }),
        );
        // Dev 3 (dynamic: second animated media + peer preview stroke).
        let rev2 = world.revision + 1;
        let seq2 = world.next_sequence(dev(3)).unwrap();
        world
            .apply(&AcceptedOp {
                revision: rev2,
                op: WorldOp {
                    id: OpId::new(&hex(8002)).unwrap(),
                    canvas: cid(),
                    author: owner(3),
                    device: dev(3),
                    sequence: seq2,
                    authority_epoch: 1,
                    action: Edit::PlaceMedia { media: Box::new(make_media(902, 3, [70.0, 50.0])) },
                },
            })
            .unwrap();

        let index = WorldSceneIndex::new(&world).unwrap();
        let view = Viewport { camera: coordinates::home(cid()), width: 256, height: 256 };
        let lod = select_target_lod(&view, 1.0, 144, None).unwrap();
        let tile = TileKey::new(lod, 0, 0).unwrap();

        let local_erase = make_brush_stroke(50, 1, 0, "Soft Round", vec![[60.0, 50.0], [95.0, 75.0]], |b| {
            b.size = 26.0;
            b.opacity = 0.7;
            b.erase = true;
        });
        let peer_stroke = make_brush_stroke(51, 3, 0, "Hard Round", vec![[75.0, 55.0], [115.0, 85.0]], |b| {
            b.size = 18.0;
            b.mode = BlendMode::Overlay;
            b.color = [0.9, 0.7, 0.1, 1.0];
        });
        let mut live_tile_brush = LiveTileBrush::new(&local_erase, lod).unwrap();
        let live_raster = live_tile_brush.raster_for_tile(tile).unwrap().unwrap();
        let previews = vec![
            SharedStroke { stroke: local_erase.clone(), device: dev(1), provenance: None, visible: true, created_revision: 0, changed_revision: 0 },
            SharedStroke { stroke: peer_stroke, device: dev(3), provenance: None, visible: true, created_revision: 0, changed_revision: 0 },
        ];

        let green = Arc::new(RgbaFrame { width: 2, height: 2, rgba: [0, 220, 40, 255].repeat(4) });
        let purple = Arc::new(RgbaFrame { width: 2, height: 2, rgba: [180, 40, 220, 255].repeat(4) });
        let mut split_cache = DynamicSplitCache::new(24 * 1024 * 1024);

        for elapsed_ms in [0u64, 120] {
            let mut resolve = |frame: &MediaFrame| -> crate::Result<Option<Arc<RgbaFrame>>> {
                if frame.hash == f1_hash { Ok(Some(Arc::clone(&green))) } else { Ok(Some(Arc::clone(&purple))) }
            };
            let params_direct = TileComposeParams {
                index: &index,
                tile,
                scope_gen: 1,
                validated_seq: 1,
                elapsed_ms,
                previews: &previews,
                live_brush: None,
                allow_parallel_brush: false,
                cancel: None,
            };
            let direct = compose_world_tile(&params_direct, None, &mut resolve).unwrap();

            let params_split = TileComposeParams {
                index: &index,
                tile,
                scope_gen: 1,
                validated_seq: 1,
                elapsed_ms,
                previews: &previews,
                live_brush: Some((local_erase.id, live_raster.1)),
                allow_parallel_brush: false,
                cancel: None,
            };
            let split = compose_world_tile(&params_split, Some(&mut split_cache), &mut resolve).unwrap();
            assert_eq!(direct.rgba8, split.rgba8, "DynamicSplitCache + LiveTileBrush diverged at elapsed_ms={elapsed_ms}");
            assert!(split_cache.used_bytes() <= 24 * 1024 * 1024);
        }
    }

    #[test]
    fn scope_purge_lazy_dirty_validation_large_dirty_rect_and_disk_cache_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let mut world = WorldState::new(cid(), owner(0), "Invalidation".into()).unwrap();
        append_stroke(
            &mut world,
            0,
            make_brush_stroke(1, 0, 0, "Hard Round", vec![[30.0, 30.0], [80.0, 80.0]], |b| {
                b.size = 16.0;
                b.color = [1.0, 0.0, 0.0, 1.0];
            }),
        );
        let mut index = WorldSceneIndex::new(&world).unwrap();
        let mut mgr = WorldTileManager::new(TileBudget::conservative(), Some(tmp.path().to_path_buf()));
        let scope1 = ScopeKey::new(1, "https://a.invalid", owner(0).as_str(), dev(0).as_str(), cid(), None);
        mgr.set_scope(scope1, Arc::new(index.clone()));

        let view = Viewport { camera: coordinates::home(cid()), width: 256, height: 256 };
        let lod = select_target_lod(&view, 1.0, 72, None).unwrap();
        let t_local = TileKey::new(lod, 0, 0).unwrap();
        let t_far = TileKey::new(lod, 5, 5).unwrap();
        mgr.render_static_tile_blocking(t_local, 0, &mut |_| Ok(None)).unwrap();
        mgr.render_static_tile_blocking(t_far, 0, &mut |_| Ok(None)).unwrap();

        // Incremental edit inside (0, 0): t_local invalidates, t_far MUST stay 100% valid!
        let new_stroke = make_brush_stroke(2, 0, 0, "Hard Round", vec![[40.0, 40.0], [90.0, 40.0]], |b| {
            b.size = 12.0;
            b.color = [0.0, 0.0, 1.0, 1.0];
        });
        let op = AcceptedOp {
            revision: 2,
            op: WorldOp {
                id: OpId::new(&hex(20_002)).unwrap(),
                canvas: cid(),
                author: owner(0),
                device: dev(0),
                sequence: 2,
                authority_epoch: 1,
                action: Edit::AddStroke { stroke: new_stroke },
            },
        };
        let dirty_rect = index.apply_accepted(&op).unwrap().unwrap();
        mgr.update_scene(Arc::new(index.clone()), Some(&[dirty_rect]));
        assert!(mgr.get_valid_cpu_tile(t_local).is_none(), "intersecting tile must invalidate");
        assert!(mgr.get_valid_cpu_tile(t_far).is_some(), "unaffected tile must remain valid");
        assert!(mgr.stats().preserved_tiles_validated >= 1);

        // Large dirty region touching 1000+ tiles: O(1) push, unaffected tile outside rect still survives!
        let giant_rect = WorldRect::new(
            -100_000 * coordinates::local_unit(0).unwrap(),
            -100_000 * coordinates::local_unit(0).unwrap(),
            500 * coordinates::local_unit(0).unwrap(),
            500 * coordinates::local_unit(0).unwrap(),
        );
        let t_outside = TileKey::new(lod, 10, 10).unwrap();
        mgr.render_static_tile_blocking(t_outside, 0, &mut |_| Ok(None)).unwrap();
        mgr.update_scene(Arc::new(index.clone()), Some(&[giant_rect]));
        assert!(mgr.get_valid_cpu_tile(t_outside).is_some(), "tile outside giant dirty rect must survive");

        // Scope change MUST immediately purge all cached tiles!
        let scope2 = ScopeKey::new(2, "https://b.invalid", owner(1).as_str(), dev(1).as_str(), cid(), None);
        mgr.set_scope(scope2, Arc::new(index.clone()));
        assert_eq!(mgr.stats().cpu_cache_tiles, 0);

        // Disk cache hit & corrupt-file recovery.
        mgr.render_static_tile_blocking(t_local, 0, &mut |_| Ok(None)).unwrap();
        let disk_files: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().flatten().map(|e| e.path()).collect();
        assert!(!disk_files.is_empty(), "disk tile cache should write compressed .ktile");
        // Corrupt every .ktile file on disk and verify fresh manager recomputes cleanly without panic/error!
        for path in disk_files {
            std::fs::write(&path, b"CORRUPT_GARBAGE_BYTES").unwrap();
        }
        let mut mgr2 = WorldTileManager::new(TileBudget::conservative(), Some(tmp.path().to_path_buf()));
        let scope3 = ScopeKey::new(3, "https://b.invalid", owner(1).as_str(), dev(1).as_str(), cid(), None);
        mgr2.set_scope(scope3, Arc::new(index));
        let recomputed = mgr2.render_static_tile_blocking(t_local, 0, &mut |_| Ok(None)).unwrap();
        assert_eq!(recomputed.rgba8.len(), (TILE_PHYSICAL * TILE_PHYSICAL * 4) as usize);

        // Regression test (Fix 6): cancelling queued/in-flight tiles via pan-away must release
        // `active_jobs` so panning back re-enqueues and completes those tiles cleanly!
        let mut mgr3 = WorldTileManager::new(TileBudget::conservative(), None);
        let scope4 = ScopeKey::new(4, "https://b.invalid", owner(1).as_str(), dev(1).as_str(), cid(), None);
        let idx3 = Arc::new(WorldSceneIndex::new(&world).unwrap());
        mgr3.set_scope(scope4, idx3);
        let view_a = photocraft_scene::Viewport { camera: coordinates::home(cid()), width: 512, height: 512 };
        let view_b = photocraft_scene::Viewport { camera: coordinates::pan(&coordinates::home(cid()), [4000.0, 4000.0]).unwrap(), width: 512, height: 512 };
        let lod = photocraft_scene::spatial::select_target_lod(&view_a, 1.0, 48, None).unwrap();
        let vis_a = photocraft_scene::spatial::visible_tiles(&view_a, lod, 48).unwrap();
        let vis_b = photocraft_scene::spatial::visible_tiles(&view_b, lod, 48).unwrap();
        mgr3.schedule_view(&view_a, lod, &vis_a, &vis_a);
        // Immediately pan away before workers finish, cancelling vis_a jobs.
        mgr3.schedule_view(&view_b, lod, &vis_b, &vis_b);
        // Pan back to view_a and ensure EVERY tile in vis_a completes cleanly!
        mgr3.schedule_view(&view_a, lod, &vis_a, &vis_a);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let _ = mgr3.poll_completed();
            if vis_a.iter().all(|&t| mgr3.get_valid_cpu_tile(t).is_some()) {
                break;
            }
            mgr3.schedule_view(&view_a, lod, &vis_a, &[]);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(vis_a.iter().all(|&t| mgr3.get_valid_cpu_tile(t).is_some()), "cancelled tiles must re-enqueue and settle on pan-back");
    }

    #[test]
    fn async_dynamic_tiles_preserve_ordering_coalesce_and_fence_stale_results() {
        let mut world = WorldState::new(cid(), owner(0), "Async Anim Media".into()).unwrap();

        // Two overlapping animated media objects (different durations: 100ms & 250ms) owned by two authors,
        // interleaved with an erasing stroke on author 0 and a Multiply stroke on author 1.
        let mut frames_map = std::collections::BTreeMap::new();
        let make_frame = |id: u128, rgba: [u8; 4], dur: u32, map: &mut std::collections::BTreeMap<AssetHash, Arc<RgbaFrame>>| -> MediaFrame {
            let h = AssetHash::new(&format!("{id:064x}")).unwrap();
            let mut buf = Vec::with_capacity(32 * 32 * 4);
            for _ in 0..(32 * 32) {
                buf.extend_from_slice(&rgba);
            }
            map.insert(h, Arc::new(RgbaFrame { width: 32, height: 32, rgba: buf }));
            MediaFrame { hash: h, byte_len: 256, width: 32, height: 32, duration_ms: dur }
        };

        let f0a = make_frame(1001, [240, 30, 30, 255], 100, &mut frames_map);
        let f0b = make_frame(1002, [30, 220, 40, 255], 100, &mut frames_map);
        let f1a = make_frame(2001, [30, 60, 240, 180], 250, &mut frames_map);
        let f1b = make_frame(2002, [240, 210, 20, 180], 250, &mut frames_map);

        for (idx, (writer, offset, frames)) in [(0u128, [-20.0, -20.0], vec![f0a, f0b]), (1u128, [-5.0, -5.0], vec![f1a, f1b])].into_iter().enumerate() {
            let rev = world.revision + 1;
            let seq = world.next_sequence(dev(writer)).unwrap();
            let mid = MediaId::new(&hex(700 + idx as u128)).unwrap();
            let placement = MediaPlacement {
                id: mid,
                canvas: cid(),
                owner: owner(writer),
                anchor: RegionKey::default(),
                offset,
                width: 60.0,
                height: 60.0,
                frames,
                looping: true,
                provenance: Provenance {
                    source: MediaSource::Canvas { canvas: CanvasId::new(&hex(99)).unwrap(), media: mid },
                    source_revision: 1,
                    artifact_hash: AssetHash::new(&format!("{:064x}", 900 + idx as u128)).unwrap(),
                    authors: vec![Credit { profile: owner(writer), label: format!("A{writer}") }],
                },
            };
            world
                .apply(&AcceptedOp {
                    revision: rev,
                    op: WorldOp {
                        id: OpId::new(&hex(30_000 + u128::from(rev))).unwrap(),
                        canvas: cid(),
                        author: owner(writer),
                        device: dev(writer),
                        sequence: seq,
                        authority_epoch: 1,
                        action: Edit::PlaceMedia { media: Box::new(placement) },
                    },
                })
                .unwrap();
        }
        // Author 0 erases across their own animated media; Author 1 paints Multiply over both.
        let mut erase_s = make_brush_stroke(50, 0, 0, "Hard Round", vec![[-15.0, 0.0], [35.0, 0.0]], |b| b.size = 14.0);
        erase_s.erase = true;
        append_stroke(&mut world, 0, erase_s);
        let mul_s = make_brush_stroke(51, 1, 0, "Soft Round", vec![[-10.0, -10.0], [30.0, 30.0]], |b| {
            b.size = 18.0;
            b.color = [0.2, 0.7, 0.9, 0.8];
            b.mode = BlendMode::Multiply;
        });
        append_stroke(&mut world, 1, mul_s);

        let peer_preview = SharedStroke {
            stroke: make_brush_stroke(99, 2, 0, "Hard Round", vec![[-10.0, 15.0], [35.0, 15.0]], |b| {
                b.size = 10.0;
                b.color = [0.9, 0.1, 0.8, 0.9];
            }),
            device: dev(2),
            provenance: None,
            visible: true,
            created_revision: 0,
            changed_revision: 0,
        };

        let index = Arc::new(WorldSceneIndex::new(&world).unwrap());
        let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);
        let scope = ScopeKey::new(1, "https://a.invalid", owner(0).as_str(), dev(0).as_str(), cid(), None);
        mgr.set_scope(scope, Arc::clone(&index));
        for (h, f) in &frames_map {
            mgr.register_media_frame(*h, Arc::clone(f));
        }

        let view = Viewport { camera: coordinates::home(cid()), width: 256, height: 256 };
        let lod = select_target_lod(&view, 1.0, 144, None).unwrap();
        let tile = TileKey::new(lod, 0, 0).unwrap();
        let active = std::collections::BTreeSet::from([tile]);
        // Enqueue superseded sig 111 then immediately replace with sig 222 at t=150ms (Media0 frame 1, Media1 frame 0).
        mgr.schedule_dynamic_tiles(active.clone(), &[(tile, 111, 0)], std::slice::from_ref(&peer_preview));
        mgr.schedule_dynamic_tiles(active.clone(), &[(tile, 222, 150)], std::slice::from_ref(&peer_preview));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let _ = mgr.poll_completed();
            if mgr.get_valid_dynamic_tile(tile, 222).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let async_tile = mgr.get_valid_dynamic_tile(tile, 222).expect("async dynamic tile must settle for sig 222");

        // Must match synchronous single-pass composition bit-identically!
        let expected = compose_world_tile(
            &TileComposeParams {
                index: &index,
                tile,
                scope_gen: mgr.scope_gen(),
                validated_seq: mgr.edit_seq(),
                elapsed_ms: 150,
                previews: std::slice::from_ref(&peer_preview),
                live_brush: None,
                allow_parallel_brush: false,
                cancel: None,
            },
            None,
            &mut |mf| Ok(frames_map.get(&mf.hash).cloned()),
        )
        .unwrap();
        assert_eq!(*async_tile.rgba8, expected.rgba8, "async dynamic tile must match exact W4 single-pass composition");

        // Stale-result fencing on dirty edit: invalidate tile rect and verify old sig 222 is rejected.
        mgr.update_scene(Arc::clone(&index), Some(&[tile.physical_rect()]));
        assert!(mgr.get_valid_dynamic_tile(tile, 222).is_none(), "dirty edit must fence prior dynamic tile");
        assert!(mgr.get_any_valid_dynamic_tile(tile).is_none(), "dirty edit must fence get_any_valid_dynamic_tile");

        // Stale-result fencing on scope switch: purge must clear dynamic cache completely.
        mgr.schedule_dynamic_tiles(active, &[(tile, 333, 300)], &[]);
        mgr.set_scope(ScopeKey::new(2, "https://b.invalid", owner(1).as_str(), dev(1).as_str(), cid(), None), index);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let _ = mgr.poll_completed();
        assert!(mgr.get_valid_dynamic_tile(tile, 333).is_none(), "scope switch must drop in-flight dynamic results");
        assert_eq!(mgr.stats().dynamic_tile_cache_bytes, 0);
    }

    #[test]
    fn cold_tile_live_draw_avoids_ui_static_band_raster_and_converges_to_exact_w4() {
        let mut world = WorldState::new(cid(), owner(0), "Cold Live Split".into()).unwrap();
        // Device 1 (local author): committed red horizontal stroke.
        append_stroke(
            &mut world,
            1,
            make_brush_stroke(1, 1, 0, "Hard Round", vec![[10.0, 40.0], [110.0, 40.0]], |b| {
                b.size = 22.0;
                b.color = [0.90, 0.15, 0.15, 1.0];
            }),
        );
        // Device 5 (> Device 1 in W4 order): dense committed blue strokes that MUST remain on top of Device 1.
        for i in 0..40_u128 {
            let y = 15.0 + (i % 10) as f64 * 8.0;
            append_stroke(
                &mut world,
                5,
                make_brush_stroke(100 + i, 5, 0, "Hard Round", vec![[30.0, y], [90.0, y + 6.0]], |b| {
                    b.size = 16.0;
                    b.color = [0.10, 0.25, 0.90, 1.0];
                }),
            );
        }

        let index = Arc::new(WorldSceneIndex::new(&world).unwrap());
        let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);
        let scope = ScopeKey::new(1, "https://cold.invalid", owner(1).as_str(), dev(1).as_str(), cid(), None);
        mgr.set_scope(scope, Arc::clone(&index));
        let view = Viewport { camera: coordinates::home(cid()), width: 256, height: 256 };
        let lod = select_target_lod(&view, 1.0, 144, None).unwrap();
        let tile = TileKey::new(lod, 0, 0).unwrap();

        // Pre-populate ONLY static cpu_cache (DynamicSplitCache stays 100% cold!).
        let _static_tile = mgr.render_static_tile_blocking(tile, 0, &mut |_| Ok(None)).unwrap();
        assert_eq!(mgr.stats().dynamic_split_bytes, 0, "DynamicSplitCache must start cold");

        // Case 1: Live green Normal stroke on Device 1 (< Device 5).
        let live_green = make_brush_stroke(500, 1, 0, "Hard Round", vec![[20.0, 20.0], [100.0, 80.0]], |b| {
            b.size = 26.0;
            b.color = [0.10, 0.85, 0.20, 1.0];
        });
        let green_preview =
            SharedStroke { stroke: live_green.clone(), device: dev(1), provenance: None, visible: true, created_revision: 999, changed_revision: 999 };
        let mut lb = LiveTileBrush::new(&live_green, tile.lod).unwrap();
        lb.prepare_tiles(&[tile]).unwrap();
        let (_, green_raster) = lb.get_prepared_tile(tile).unwrap();

        let cold_res = mgr
            .render_live_tiles_nonblocking(
                &[(tile, 0, false, Some((live_green.id, green_raster)))],
                live_green.id,
                0,
                std::slice::from_ref(&green_preview),
                &mut |_| Ok(None),
            )
            .unwrap();
        assert!(cold_res[0].1.used_cold_split_fallback, "cold tile with devices > local_dev must NOT rasterize static bands on UI thread");
        assert_eq!(mgr.stats().dynamic_split_bytes, 0, "UI-thread non-blocking path must leave cold split cache untouched");

        // Schedule off-thread WarmSplitBands and wait for completion.
        let before_gen = mgr.split_warm_gen();
        let d1_arc: Arc<[DeviceId]> = Arc::from([dev(1)]);
        mgr.schedule_split_warm_tiles(std::collections::BTreeSet::from([tile]), &[(tile, 777, 0, Arc::clone(&d1_arc))], &[]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let _ = mgr.poll_completed();
            if mgr.split_warm_gen() > before_gen {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(mgr.split_warm_gen() > before_gen, "WarmSplitBands worker must complete");
        assert!(mgr.stats().dynamic_split_bytes > 0, "DynamicSplitCache must now be warm");

        // Warm call must NOT use fallback and must match exact single-pass W4 composition bit-identically!
        let warm_res = mgr
            .render_live_tiles_nonblocking(
                &[(tile, 0, false, Some((live_green.id, green_raster)))],
                live_green.id,
                0,
                std::slice::from_ref(&green_preview),
                &mut |_| Ok(None),
            )
            .unwrap();
        assert!(!warm_res[0].1.used_cold_split_fallback, "warm DynamicSplitCache must produce exact W4 result immediately");
        let expected_green = compose_world_tile(
            &TileComposeParams {
                index: &index,
                tile,
                scope_gen: mgr.scope_gen(),
                validated_seq: mgr.edit_seq(),
                elapsed_ms: 0,
                previews: std::slice::from_ref(&green_preview),
                live_brush: Some((live_green.id, green_raster)),
                allow_parallel_brush: false,
                cancel: None,
            },
            None,
            &mut |_| Ok(None),
        )
        .unwrap();
        assert_eq!(warm_res[0].1.rgba8, expected_green.rgba8, "warm non-blocking live tile must match exact W4 order bit-identically");

        // Case 2: Live Erase on Device 1 (MUST erase Device 1 red while leaving Device 5 blue 100% intact,
        // and on a cold tile must NEVER falsely erase Device 5 during temporary fallback!).
        mgr.update_scene(Arc::clone(&index), Some(&[tile.physical_rect()]));
        let static_again = mgr.render_static_tile_blocking(tile, 0, &mut |_| Ok(None)).unwrap();
        let live_erase = make_brush_stroke(501, 1, 0, "Hard Round", vec![[10.0, 40.0], [110.0, 40.0]], |b| {
            b.size = 34.0;
            b.erase = true;
        });
        let erase_preview =
            SharedStroke { stroke: live_erase.clone(), device: dev(1), provenance: None, visible: true, created_revision: 1000, changed_revision: 1000 };
        let mut lb_e = LiveTileBrush::new(&live_erase, tile.lod).unwrap();
        lb_e.prepare_tiles(&[tile]).unwrap();
        let (_, erase_raster) = lb_e.get_prepared_tile(tile).unwrap();

        let cold_erase = mgr
            .render_live_tiles_nonblocking(
                &[(tile, 0, false, Some((live_erase.id, erase_raster)))],
                live_erase.id,
                0,
                std::slice::from_ref(&erase_preview),
                &mut |_| Ok(None),
            )
            .unwrap();
        assert!(cold_erase[0].1.used_cold_split_fallback, "cold erase tile must defer split prep off-thread");
        assert_eq!(cold_erase[0].1.rgba8, *static_again.rgba8, "cold erase fallback must hold base tile untouched rather than falsely erasing peers");

        let before_gen2 = mgr.split_warm_gen();
        mgr.schedule_split_warm_tiles(std::collections::BTreeSet::from([tile]), &[(tile, 888, 0, d1_arc)], &[]);
        let deadline2 = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline2 {
            let _ = mgr.poll_completed();
            if mgr.split_warm_gen() > before_gen2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(mgr.split_warm_gen() > before_gen2);
        let warm_erase = mgr
            .render_live_tiles_nonblocking(
                &[(tile, 0, false, Some((live_erase.id, erase_raster)))],
                live_erase.id,
                0,
                std::slice::from_ref(&erase_preview),
                &mut |_| Ok(None),
            )
            .unwrap();
        assert!(!warm_erase[0].1.used_cold_split_fallback);
        let expected_erase = compose_world_tile(
            &TileComposeParams {
                index: &index,
                tile,
                scope_gen: mgr.scope_gen(),
                validated_seq: mgr.edit_seq(),
                elapsed_ms: 0,
                previews: std::slice::from_ref(&erase_preview),
                live_brush: Some((live_erase.id, erase_raster)),
                allow_parallel_brush: false,
                cancel: None,
            },
            None,
            &mut |_| Ok(None),
        )
        .unwrap();
        assert_eq!(warm_erase[0].1.rgba8, expected_erase.rgba8, "warm live erase must match exact W4 author-isolated erase bit-identically");
    }
}
