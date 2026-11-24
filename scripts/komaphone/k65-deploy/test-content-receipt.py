#!/usr/bin/env python3
"""Synthetic, local-only populated recovery receipt regressions."""
import contextlib
import copy
import importlib.util
import json
import pathlib
import shutil
import sqlite3
import tempfile
import unittest
import zlib

spec = importlib.util.spec_from_file_location("receipt", pathlib.Path(__file__).with_name("content-receipt.py"))
receipt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(receipt)
ROOM, GAME, CANVAS, AUTHOR = (f"{number:032x}" for number in range(1, 5))
PRIVATE = "PRIVATE_LABEL_OR_AUTHORITY_MUST_NOT_APPEAR"


class ContentReceiptTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.database = pathlib.Path(self.temporary.name) / "fixture.sqlite3"
        image = b"synthetic accepted image bytes"
        asset = {"hash": receipt.sha(image), "bytes": len(image)}
        value = {"assignment": {"game": GAME, "round": 0, "chain": 0}, "author": AUTHOR,
                 "assets": [asset], "image": asset, "note": PRIVATE}
        state = {"id": ROOM, "game": GAME, "revision": 8, "phase": "reveal",
                 "entries": [{"kind": "submitted", "value": value}], "private_label": PRIVATE,
                 "members": [{"connected": True, "private_label": PRIVATE}], "waiting": [], "last_now_ms": 100}
        world = {"canvas": CANVAS, "revision": 1, "name": PRIVATE, "strokes": {"s1": {"points": [1, 2]}}, "media": {}, "tours": {}}
        with contextlib.closing(sqlite3.connect(self.database)) as db:
            with db:
                db.executescript('''CREATE TABLE rooms(id TEXT PRIMARY KEY,snapshot BLOB);
                CREATE TABLE archives(room TEXT,revision INTEGER,snapshot BLOB,PRIMARY KEY(room,revision));
                CREATE TABLE objects(id INTEGER PRIMARY KEY,room TEXT,actor TEXT,hash TEXT,total INTEGER,received INTEGER,complete INTEGER,data BLOB);
                CREATE TABLE worlds(id TEXT PRIMARY KEY,revision INTEGER,state BLOB,hash TEXT);
                CREATE TABLE world_events(canvas TEXT,revision INTEGER,body BLOB,hash TEXT,PRIMARY KEY(canvas,revision));
                CREATE TABLE world_members(canvas TEXT,profile TEXT,epoch INTEGER,active INTEGER,PRIMARY KEY(canvas,profile));
                CREATE TABLE world_media_sources(source_key TEXT PRIMARY KEY,body BLOB,hash TEXT);
                CREATE TABLE world_blobs(canvas TEXT,hash TEXT,total INTEGER,data BLOB,PRIMARY KEY(canvas,hash));
                CREATE TABLE world_trash(canvas TEXT PRIMARY KEY,deleted_at_ms INTEGER,owner_epoch INTEGER);
                CREATE TABLE receipts(actor TEXT,id TEXT,hash TEXT,response BLOB,PRIMARY KEY(actor,id));''')
            with db:
                db.execute("INSERT INTO rooms VALUES(?,?)", (ROOM, receipt.encoded(state)))
                db.execute("INSERT INTO archives VALUES(?,?,?)", (ROOM, 8, receipt.encoded(state)))
                db.execute("INSERT INTO objects VALUES(1,?,?,?,?,?,?,?)", (ROOM, AUTHOR, asset["hash"], len(image), len(image), 1, image))
                body = receipt.encoded(world)
                db.execute("INSERT INTO worlds VALUES(?,?,?,?)", (CANVAS, 1, body, receipt.sha(body)))
                event = b'{"revision":1}'
                db.execute("INSERT INTO world_events VALUES(?,?,?,?)", (CANVAS, 1, event, receipt.sha(event)))
                db.execute("INSERT INTO world_members VALUES(?,?,1,1)", (CANVAS, AUTHOR))
                source = b'{"synthetic_source":1}'
                db.execute("INSERT INTO world_media_sources VALUES(?,?,?)", ("source", source, receipt.sha(source)))
                db.execute("INSERT INTO world_blobs VALUES(?,?,?,?)", (CANVAS, receipt.sha(image), len(image), image))
                db.execute("INSERT INTO world_trash VALUES(?,1,2)", (CANVAS,))
                db.execute("INSERT INTO receipts VALUES(?,?,?,?)", (AUTHOR, "request", "hash", PRIVATE.encode()))
        export = {"room": ROOM, "game": GAME, "authors": 1, "acceptedEntries": 1, "files": [PRIVATE],
                  "art": [{"assignment": value["assignment"], "image": asset}]}
        self.expected = {"schema": 1, "games": [receipt.game_expectation(export)],
                         "canvases": [{"canvas": CANVAS, "revision": 1, "stroke_count": 1}]}

    def inspect(self):
        return receipt.inspect(self.database, self.expected, True)

    def test_populated_content_copy_matches_and_receipt_does_not_disclose_data(self):
        before = receipt.sha(self.database.read_bytes())
        observed = self.inspect()
        self.assertEqual(len(observed["tables"]), 10)
        self.assertEqual(observed["matched_games"], [GAME])
        self.assertEqual(observed["matched_canvases"], [CANVAS])
        self.assertNotIn(PRIVATE, json.dumps(observed))
        self.assertNotIn(AUTHOR, json.dumps(observed))
        self.assertEqual(before, receipt.sha(self.database.read_bytes()))
        destination = self.database.with_name("restored.sqlite3")
        shutil.copyfile(self.database, destination)
        self.assertEqual(observed, receipt.inspect(destination, self.expected, True))

    def test_previously_uncompared_content_changes_are_detected(self):
        before = self.inspect()
        changes = {
            "world_media_sources": ("UPDATE world_media_sources SET body=?,hash=?", (b"changed", receipt.sha(b"changed"))),
            "world_blobs": ("UPDATE world_blobs SET data=?,hash=?,total=?", (b"changed", receipt.sha(b"changed"), 7)),
            "world_trash": ("UPDATE world_trash SET owner_epoch=3", ()),
            "receipts": ("UPDATE receipts SET response=?", (b"changed",)),
        }
        for table, (sql, args) in changes.items():
            with self.subTest(table=table):
                with contextlib.closing(sqlite3.connect(self.database)) as db:
                    with db:
                        db.execute(sql, args)
                self.assertNotEqual(before["tables"][table], self.inspect()["tables"][table])

    def test_corrupt_completed_object_is_rejected(self):
        with contextlib.closing(sqlite3.connect(self.database)) as db:
            with db:
                db.execute("UPDATE objects SET data=zeroblob(total)")
        with self.assertRaisesRegex(ValueError, "artwork hash"):
            self.inspect()

    def test_unacknowledged_art_hash_is_rejected(self):
        self.expected["games"][0]["art"][0]["hash"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "game content"):
            self.inspect()

    def test_canvas_revision_or_event_loss_is_rejected(self):
        self.expected["canvases"][0]["revision"] = 2
        with self.assertRaisesRegex(ValueError, "canvas content"):
            self.inspect()
        self.expected["canvases"][0]["revision"] = 1
        with contextlib.closing(sqlite3.connect(self.database)) as db:
            with db:
                db.execute("DELETE FROM world_events")
        with self.assertRaisesRegex(ValueError, "event revision"):
            self.inspect()

    def test_partial_preallocated_object_does_not_claim_completed_art(self):
        with contextlib.closing(sqlite3.connect(self.database)) as db:
            with db:
                db.execute("INSERT INTO objects VALUES(2,?,?,?,?,?,?,?)", (ROOM, AUTHOR, "0" * 64, 16, 3, 0, b"abc" + b"\0" * 13))
        self.assertEqual(self.inspect()["complete_objects"], 1)

    def test_population_and_complete_acknowledgements_required(self):
        with self.assertRaisesRegex(ValueError, "expectations required"):
            receipt.inspect(self.database, None, True)
        self.expected["canvases"] = []
        with self.assertRaisesRegex(ValueError, "expectations required"):
            self.inspect()

    def test_export_receipt_allowlist_discards_extra_private_fields(self):
        expected = copy.deepcopy(self.expected["games"][0])
        self.assertNotIn(PRIVATE, json.dumps(expected))
        self.assertEqual(set(expected), {"room", "game", "accepted_entries", "authors", "art"})

    def test_exact_restart_changes_accepted_but_art_changes_rejected(self):
        before = self.inspect()
        restored = self.database.with_name("restarted.sqlite3")
        shutil.copyfile(self.database, restored)
        with contextlib.closing(sqlite3.connect(restored)) as db:
            with db:
                state = json.loads(db.execute("SELECT snapshot FROM rooms").fetchone()[0])
                state["revision"] += 1
                state["last_now_ms"] = 250
                state["members"][0]["connected"] = False
                db.execute("UPDATE rooms SET snapshot=?", (receipt.encoded(state),))
        after = receipt.inspect(restored, self.expected, True)
        self.assertEqual(receipt.compare_restored(self.database, restored, before, after)["validated_room_restart_transitions"], 1)
        with contextlib.closing(sqlite3.connect(restored)) as db:
            with db:
                state["entries"][0]["value"]["note"] = "changed artwork attribution"
                db.execute("UPDATE rooms SET snapshot=?", (receipt.encoded(state),))
        with self.assertRaisesRegex(ValueError, "beyond exact restart"):
            receipt.compare_restored(self.database, restored, before, receipt.inspect(restored, self.expected, True))

    def test_restart_reconnect_requires_exact_presence_revision_increment(self):
        before = self.inspect()
        restored = self.database.with_name("reconnected.sqlite3")
        shutil.copyfile(self.database, restored)
        with contextlib.closing(sqlite3.connect(restored)) as db:
            with db:
                state = json.loads(db.execute("SELECT snapshot FROM rooms").fetchone()[0])
                state["revision"] += 2  # One restart, one member reconnect.
                state["last_now_ms"] = 250
                db.execute("UPDATE rooms SET snapshot=?", (receipt.encoded(state),))
        after = receipt.inspect(restored, self.expected, True)
        self.assertTrue(receipt.compare_restored(self.database, restored, before, after, True)["reconnect_presence_transitions_allowed"])
        with contextlib.closing(sqlite3.connect(restored)) as db:
            with db:
                state["revision"] += 1
                db.execute("UPDATE rooms SET snapshot=?", (receipt.encoded(state),))
        with self.assertRaisesRegex(ValueError, "beyond exact restart"):
            receipt.compare_restored(self.database, restored, before, receipt.inspect(restored, self.expected, True), True)

    def test_schema6_compressed_and_raw_events_chain_hash_and_tamper_rejection(self):
        s6_db = self.database.with_name("schema6.sqlite3")
        ev1 = b'{"revision":1,"kind":"raw"}'
        ev2 = (b'{"revision":2,"kind":"compressed","pad":"' + b"x" * 600 + b'"}')
        ev2_stored = receipt.EVENT_ZLIB_MAGIC + zlib.compress(ev2, 5)
        chain = receipt.chain_hash_init(CANVAS, AUTHOR, PRIVATE)
        chain = receipt.chain_hash_step(chain, 1, receipt.sha(ev1))
        chain = receipt.chain_hash_step(chain, 2, receipt.sha(ev2))
        image = b"synthetic accepted image bytes"
        asset = {"hash": receipt.sha(image), "bytes": len(image)}
        value = {"assignment": {"game": GAME, "round": 0, "chain": 0}, "author": AUTHOR,
                 "assets": [asset], "image": asset, "note": PRIVATE}
        state = {"id": ROOM, "game": GAME, "revision": 8, "phase": "reveal",
                 "entries": [{"kind": "submitted", "value": value}], "private_label": PRIVATE,
                 "members": [{"connected": True, "private_label": PRIVATE}], "waiting": [], "last_now_ms": 100}
        with contextlib.closing(sqlite3.connect(s6_db)) as db:
            with db:
                db.executescript('''CREATE TABLE world_schema(version INTEGER NOT NULL);
                    CREATE TABLE rooms(id TEXT PRIMARY KEY,snapshot BLOB);
                    CREATE TABLE archives(room TEXT,revision INTEGER,snapshot BLOB,PRIMARY KEY(room,revision));
                    CREATE TABLE objects(id INTEGER PRIMARY KEY,room TEXT,actor TEXT,hash TEXT,total INTEGER,received INTEGER,complete INTEGER,data BLOB);
                    CREATE TABLE worlds(id TEXT PRIMARY KEY,owner TEXT,name TEXT,initial_name TEXT,revision INTEGER,state BLOB,hash TEXT);
                    CREATE TABLE world_events(canvas TEXT,revision INTEGER,body BLOB,hash TEXT,PRIMARY KEY(canvas,revision));
                    CREATE TABLE world_members(canvas TEXT,profile TEXT,epoch INTEGER,active INTEGER,PRIMARY KEY(canvas,profile));
                    CREATE TABLE world_media_sources(source_key TEXT PRIMARY KEY,body BLOB,hash TEXT);
                    CREATE TABLE world_blobs(canvas TEXT,hash TEXT,total INTEGER,data BLOB,PRIMARY KEY(canvas,hash));
                    CREATE TABLE world_trash(canvas TEXT PRIMARY KEY,deleted_at_ms INTEGER,owner_epoch INTEGER);
                    CREATE TABLE receipts(actor TEXT,id TEXT,hash TEXT,response BLOB,PRIMARY KEY(actor,id));
                    CREATE TABLE world_meta(canvas TEXT PRIMARY KEY,schema_ver INTEGER,metadata_revision INTEGER,chain_hash TEXT,limits_json BLOB,stroke_count INTEGER,point_count INTEGER,media_count INTEGER,media_frame_count INTEGER,tour_count INTEGER);
                    CREATE TABLE world_devices(canvas TEXT,device TEXT,author TEXT,last_sequence INTEGER,PRIMARY KEY(canvas,device));
                    CREATE TABLE world_strokes_idx(canvas TEXT,stroke TEXT,owner TEXT,device TEXT,points INTEGER,visible INTEGER,created_revision INTEGER,changed_revision INTEGER,PRIMARY KEY(canvas,stroke));
                    CREATE TABLE world_media_idx(canvas TEXT,media TEXT,owner TEXT,device TEXT,frame_count INTEGER,visible INTEGER,created_revision INTEGER,changed_revision INTEGER,PRIMARY KEY(canvas,media));
                    CREATE TABLE world_tours_idx(canvas TEXT,tour TEXT,revision INTEGER,present INTEGER,PRIMARY KEY(canvas,tour));
                    CREATE TABLE world_checkpoints(canvas TEXT,revision INTEGER,schema_ver INTEGER,chain_hash TEXT,state_hash TEXT,compressed_sha256 TEXT,compressed_bytes INTEGER,uncompressed_bytes INTEGER,body_zlib BLOB,PRIMARY KEY(canvas,revision));''')
                db.execute("INSERT INTO world_schema VALUES(6)")
                db.execute("INSERT INTO rooms VALUES(?,?)", (ROOM, receipt.encoded(state)))
                db.execute("INSERT INTO archives VALUES(?,?,?)", (ROOM, 8, receipt.encoded(state)))
                db.execute("INSERT INTO objects VALUES(1,?,?,?,?,?,?,?)", (ROOM, AUTHOR, asset["hash"], len(image), len(image), 1, image))
                db.execute("INSERT INTO worlds VALUES(?,?,?,?,?,?,?)", (CANVAS, AUTHOR, PRIVATE, PRIVATE, 2, b"", chain))
                db.execute("INSERT INTO world_events VALUES(?,?,?,?)", (CANVAS, 1, ev1, receipt.sha(ev1)))
                db.execute("INSERT INTO world_events VALUES(?,?,?,?)", (CANVAS, 2, ev2_stored, receipt.sha(ev2)))
                db.execute("INSERT INTO world_members VALUES(?,?,1,1)", (CANVAS, AUTHOR))
                db.execute("INSERT INTO world_meta VALUES(?,2,0,?,X'7b7d',1,2,0,0,0)", (CANVAS, chain))
                db.execute("INSERT INTO world_strokes_idx VALUES(?,?,?,?,'2',1,1,1)", (CANVAS, "s1", AUTHOR, "d1"))
        expected_s6 = copy.deepcopy(self.expected)
        expected_s6["canvases"][0]["revision"] = 2
        expected_s6["canvases"][0]["state_sha256"] = chain
        observed = receipt.inspect(s6_db, expected_s6, True)
        self.assertEqual(len(observed["tables"]), 16)
        self.assertEqual(observed["matched_canvases"], [CANVAS])
        self.assertNotIn(PRIVATE, json.dumps(observed))
        self.assertNotIn(AUTHOR, json.dumps(observed))
        with contextlib.closing(sqlite3.connect(s6_db)) as db:
            with db:
                db.execute("UPDATE world_events SET body=? WHERE revision=2", (receipt.EVENT_ZLIB_MAGIC + zlib.compress(b"{}", 5),))
        with self.assertRaisesRegex(ValueError, "event history"):
            receipt.inspect(s6_db, expected_s6, True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
