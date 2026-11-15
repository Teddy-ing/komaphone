#!/usr/bin/env python3
"""Synthetic, local-only populated recovery receipt regressions."""
import copy
import importlib.util
import json
import pathlib
import shutil
import sqlite3
import tempfile
import unittest

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
        with sqlite3.connect(self.database) as db:
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
                with sqlite3.connect(self.database) as db:
                    db.execute(sql, args)
                self.assertNotEqual(before["tables"][table], self.inspect()["tables"][table])

    def test_corrupt_completed_object_is_rejected(self):
        with sqlite3.connect(self.database) as db:
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
        with sqlite3.connect(self.database) as db:
            db.execute("DELETE FROM world_events")
        with self.assertRaisesRegex(ValueError, "event revision"):
            self.inspect()

    def test_partial_preallocated_object_does_not_claim_completed_art(self):
        with sqlite3.connect(self.database) as db:
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
        with sqlite3.connect(restored) as db:
            state = json.loads(db.execute("SELECT snapshot FROM rooms").fetchone()[0])
            state["revision"] += 1
            state["last_now_ms"] = 250
            state["members"][0]["connected"] = False
            db.execute("UPDATE rooms SET snapshot=?", (receipt.encoded(state),))
        after = receipt.inspect(restored, self.expected, True)
        self.assertEqual(receipt.compare_restored(self.database, restored, before, after)["validated_room_restart_transitions"], 1)
        with sqlite3.connect(restored) as db:
            state["entries"][0]["value"]["note"] = "changed artwork attribution"
            db.execute("UPDATE rooms SET snapshot=?", (receipt.encoded(state),))
        with self.assertRaisesRegex(ValueError, "beyond exact restart"):
            receipt.compare_restored(self.database, restored, before, receipt.inspect(restored, self.expected, True))

    def test_restart_reconnect_requires_exact_presence_revision_increment(self):
        before = self.inspect()
        restored = self.database.with_name("reconnected.sqlite3")
        shutil.copyfile(self.database, restored)
        with sqlite3.connect(restored) as db:
            state = json.loads(db.execute("SELECT snapshot FROM rooms").fetchone()[0])
            state["revision"] += 2  # One restart, one member reconnect.
            state["last_now_ms"] = 250
            db.execute("UPDATE rooms SET snapshot=?", (receipt.encoded(state),))
        after = receipt.inspect(restored, self.expected, True)
        self.assertTrue(receipt.compare_restored(self.database, restored, before, after, True)["reconnect_presence_transitions_allowed"])
        with sqlite3.connect(restored) as db:
            state["revision"] += 1
            db.execute("UPDATE rooms SET snapshot=?", (receipt.encoded(state),))
        with self.assertRaisesRegex(ValueError, "beyond exact restart"):
            receipt.compare_restored(self.database, restored, before, receipt.inspect(restored, self.expected, True), True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
