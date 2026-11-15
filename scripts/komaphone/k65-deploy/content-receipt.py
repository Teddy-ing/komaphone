#!/usr/bin/env python3
"""Read-only content receipts: IDs/counts/hashes, never snapshots or authority."""
import argparse
import copy
import hashlib
import json
import pathlib
import re
import sqlite3

TABLES = {"rooms": "id", "archives": "room,revision", "objects": "id", "worlds": "id",
          "world_events": "canvas,revision", "world_members": "canvas,profile",
          "world_media_sources": "source_key", "world_blobs": "canvas,hash",
          "world_trash": "canvas", "receipts": "actor,id"}
MAX_CELL = 64 * 1024 * 1024


def require(value, message):
    if not value:
        raise ValueError(message)


def sha(value):
    return hashlib.sha256(value).hexdigest()


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False).encode()


def identifier(value):
    require(isinstance(value, str) and re.fullmatch("[0-9a-f]{32}", value), "Invalid content identifier")
    return value


def read_json(path):
    with pathlib.Path(path).open("rb") as source:
        body = source.read(4 * 1024 * 1024 + 1)
    require(len(body) <= 4 * 1024 * 1024, "Receipt exceeds size limit")
    return json.loads(body)


def game_expectation(receipt):
    """Allowlist an export receipt; exclude filenames, identities and extra fields."""
    room, game = identifier(receipt["room"]), identifier(receipt["game"])
    art = []
    for entry in receipt["art"]:
        assignment, image = entry["assignment"], entry["image"]
        require(assignment["game"] == game, "Receipt game mismatch")
        require(type(assignment["chain"]) is int and type(assignment["round"]) is int, "Invalid assignment")
        require(type(image["bytes"]) is int and 0 < image["bytes"] <= MAX_CELL, "Invalid image size")
        require(isinstance(image["hash"], str) and re.fullmatch("[0-9a-f]{64}", image["hash"]), "Invalid image hash")
        art.append({"chain": assignment["chain"], "round": assignment["round"], "hash": image["hash"], "bytes": image["bytes"]})
    require(0 < len(art) <= 1000 and len(art) == receipt["acceptedEntries"], "Accepted art count mismatch")
    require(len({(item["chain"], item["round"]) for item in art}) == len(art), "Duplicate accepted assignment")
    return {"room": room, "game": game, "accepted_entries": len(art), "authors": receipt["authors"],
            "art": sorted(art, key=lambda item: (item["round"], item["chain"]))}


def table_receipt(database, table, order):
    columns = database.execute(f'PRAGMA table_info("{table}")').fetchall()
    require(columns, "Required content table missing")
    for column in columns:
        quoted = '"' + column[1].replace('"', '""') + '"'
        require(database.execute(f'SELECT COALESCE(MAX(length({quoted})),0) FROM "{table}"').fetchone()[0] <= MAX_CELL,
                "Content cell exceeds receipt limit")
    digest = hashlib.sha256(encoded([(row[1], row[2]) for row in columns]) + b"\n")
    count = 0
    for row in database.execute(f'SELECT * FROM "{table}" ORDER BY {order}'):
        count += 1
        require(count <= 1000000, "Content row count exceeds receipt limit")
        values = [["blob", len(value), sha(value)] if isinstance(value, bytes) else [type(value).__name__, value] for value in row]
        digest.update(encoded(values) + b"\n")
    return {"rows": count, "sha256": digest.hexdigest()}


def inspect(path, expectations=None, require_populated=False):
    path = pathlib.Path(path).resolve(strict=True)
    require(path.is_file() and path.stat().st_size <= 4 * 1024**3, "Invalid receipt database")
    with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=10) as database:
        database.execute("PRAGMA query_only=ON")
        database.execute("BEGIN")
        require(database.execute("PRAGMA quick_check").fetchone()[0] == "ok", "Database integrity failed")
        tables = {table: table_receipt(database, table, order) for table, order in TABLES.items()}
        objects = {}
        complete_bytes = complete_count = 0
        for room, actor, digest, total, received, complete, data in database.execute("SELECT room,actor,hash,total,received,complete,data FROM objects"):
            require(len(data) == total and 0 <= received <= total, "Object size mismatch")
            if complete:
                require(received == total and sha(data) == digest, "Completed artwork hash mismatch")
                objects[(room, digest)] = total
                complete_count += 1
                complete_bytes += total
        for digest, total, data in database.execute("SELECT hash,total,data FROM world_blobs"):
            require(len(data) == total and sha(data) == digest, "Canvas media blob mismatch")
        for body, digest in database.execute("SELECT body,hash FROM world_media_sources"):
            require(sha(body) == digest, "Canvas media source mismatch")
        games = {}
        for room, body in database.execute("SELECT id,snapshot FROM rooms UNION ALL SELECT room,snapshot FROM archives"):
            require(len(body) <= 4 * 1024 * 1024, "Room snapshot exceeds limit")
            state = json.loads(body)
            require(state["id"] == room, "Room identity mismatch")
            if state["game"] is None:
                continue
            game = identifier(state["game"])
            art, authors = [], set()
            for entry in state["entries"]:
                if entry["kind"] != "submitted":
                    continue
                value = entry["value"]
                require(value["assignment"]["game"] == game, "Stored assignment game mismatch")
                for asset in value["assets"]:
                    require(objects.get((room, asset["hash"])) == asset["bytes"], "Accepted submission dependency missing")
                image = value["image"]
                require(objects.get((room, image["hash"])) == image["bytes"], "Accepted image missing")
                authors.add(value["author"])
                art.append({"chain": value["assignment"]["chain"], "round": value["assignment"]["round"], "hash": image["hash"], "bytes": image["bytes"]})
            summary = {"room": identifier(room), "game": game, "revision": state["revision"], "phase": state["phase"],
                       "accepted_entries": len(art), "authors": len(authors), "snapshot_sha256": sha(body),
                       "art": sorted(art, key=lambda item: (item["round"], item["chain"]))}
            key = (room, game)
            if key not in games or games[key]["revision"] < summary["revision"]:
                games[key] = summary
        canvases = {}
        for canvas, revision, state_body, digest in database.execute("SELECT id,revision,state,hash FROM worlds ORDER BY id"):
            require(len(state_body) <= 32 * 1024 * 1024 and sha(state_body) == digest, "Canvas state hash mismatch")
            state = json.loads(state_body)
            require(state["canvas"] == canvas and state["revision"] == revision, "Canvas revision mismatch")
            event_count = 0
            for event_revision, body, event_hash in database.execute("SELECT revision,body,hash FROM world_events WHERE canvas=? ORDER BY revision", (canvas,)):
                event_count += 1
                require(event_revision == event_count and sha(body) == event_hash, "Canvas event history mismatch")
            require(event_count == revision, "Canvas event revision count mismatch")
            canvases[canvas] = {"canvas": identifier(canvas), "revision": revision, "state_sha256": digest,
                                "event_count": event_count, "stroke_count": len(state["strokes"]),
                                "stroke_ids_sha256": sha(encoded(sorted(state["strokes"]))),
                                "stroke_metadata_sha256": sha(encoded(state["strokes"])),
                                "media_count": len(state["media"]), "tour_count": len(state["tours"])}
        matched_games, matched_canvases = [], []
        if expectations is not None:
            require(expectations.get("schema") == 1, "Unknown expectation schema")
            for expected in expectations["games"]:
                key = (expected["room"], expected["game"])
                require(key in games, "Acknowledged game missing")
                actual = games[key]
                require(actual["phase"] in ("reveal", "archived"), "Acknowledged game is not terminal")
                for field in ("accepted_entries", "authors", "art"):
                    require(actual[field] == expected[field], "Acknowledged game content mismatch")
                require(actual["revision"] >= expected.get("minimum_revision", 0), "Acknowledged game revision missing")
                matched_games.append(expected["game"])
            for expected in expectations["canvases"]:
                require(expected["canvas"] in canvases, "Acknowledged canvas missing")
                actual = canvases[expected["canvas"]]
                require("revision" in expected and "stroke_count" in expected, "Canvas revision/count expectations required")
                for field, value in expected.items():
                    require(field in actual and actual[field] == value, "Acknowledged canvas content mismatch")
                matched_canvases.append(expected["canvas"])
        if require_populated:
            require(matched_games and matched_canvases and complete_count > 0, "Populated acknowledgement expectations required")
        return {"tables": tables, "games": [games[key] for key in sorted(games)], "canvases": [canvases[key] for key in sorted(canvases)],
                "complete_objects": complete_count, "complete_object_bytes": complete_bytes,
                "matched_games": matched_games, "matched_canvases": matched_canvases}


def compare_restored(source, restored, before, after, allow_reconnect=False):
    """The restore CLI opens Store once: validate its exact Room::restart effects."""
    for table in TABLES:
        if table != "rooms":
            require(before["tables"][table] == after["tables"][table], "Restored content table differs")
    with sqlite3.connect(pathlib.Path(source).resolve().as_uri() + "?mode=ro", uri=True) as original, \
         sqlite3.connect(pathlib.Path(restored).resolve().as_uri() + "?mode=ro", uri=True) as recovered:
        old_rows = original.execute("SELECT id,snapshot FROM rooms ORDER BY id").fetchall()
        new_rows = recovered.execute("SELECT id,snapshot FROM rooms ORDER BY id").fetchall()
        require(len(old_rows) == len(new_rows), "Restored room count differs")
        for (old_id, old_body), (new_id, new_body) in zip(old_rows, new_rows):
            require(old_id == new_id, "Restored room identity differs")
            old, new = json.loads(old_body), json.loads(new_body)
            expected = copy.deepcopy(old)
            expected["revision"] += 1
            require(type(new["last_now_ms"]) is int and new["last_now_ms"] >= 0, "Invalid restored clock")
            expected["last_now_ms"] = new["last_now_ms"]
            for member in expected["members"] + expected.get("waiting", []):
                member["connected"] = False
            if allow_reconnect:
                old_members = expected["members"] + expected.get("waiting", [])
                new_members = new["members"] + new.get("waiting", [])
                require(len(old_members) == len(new_members), "Membership changed during restart")
                for member, observed in zip(old_members, new_members):
                    require(type(observed["connected"]) is bool, "Invalid connection state")
                    member["connected"] = observed["connected"]
                    expected["revision"] += int(observed["connected"])
            if expected["phase"] in ("drawing", "transfer"):
                if expected["paused"] is None:
                    deadline = expected["deadline_ms"]
                    expected["remaining_ms"] = None if deadline is None else max(0, deadline - old["last_now_ms"])
                    expected["deadline_ms"] = None
                expected["paused"] = "restart"
            require(expected == new, "Restored room differs beyond exact restart transition")
    return {"identical_nonroom_tables": len(TABLES) - 1, "validated_room_restart_transitions": len(old_rows),
            "reconnect_presence_transitions_allowed": allow_reconnect,
            "all_room_art_and_permissions_preserved": True}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True, type=pathlib.Path)
    parser.add_argument("--expectations", type=pathlib.Path)
    parser.add_argument("--require-populated", action="store_true")
    args = parser.parse_args()
    print(json.dumps(inspect(args.database, read_json(args.expectations) if args.expectations else None, args.require_populated), indent=2))
