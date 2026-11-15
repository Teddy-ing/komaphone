"""Compare a real exported/imported synthetic world, allowing only fresh identities.

Test orchestration only; product and archive validation remain Rust.
"""
import argparse
import hashlib
import json
from pathlib import Path


def verify(source_dir, imported_dir):
    source = json.loads((source_dir / "world.json").read_text(encoding="utf-8-sig"))
    restored = json.loads((imported_dir / "world.json").read_text(encoding="utf-8-sig"))
    before, after = source["world"], restored["world"]
    assert before["canvas"] != after["canvas"], "Canvas identity was reused"
    origin = after["import_origin"]
    assert origin["source_canvas"] == before["canvas"]
    assert origin["source_revision"] == before["revision"] == after["revision"]
    assert origin["source_owner"] == before["owner"]
    # One blob can appear in frames with different durations. Fresh media IDs
    # can change which reference supplies the inventory's representative frame;
    # the complete world's per-frame timings are compared below without changes.
    inventory = lambda archive: {frame["hash"]: (frame["byte_len"], frame["width"], frame["height"]) for frame in archive["assets"]}
    assert inventory(source) == inventory(restored), "Immutable image descriptors changed"
    assert len(source["assets"]) == len(restored["assets"]) == len(inventory(source))
    mapping = {}
    reverse = {}

    def bind(new, old, kind):
        assert new != old, "Imported identity was not fresh"
        assert mapping.setdefault(new, old) == old, "Identity alias collision"
        assert reverse.setdefault((kind, old), new) == new, "One source identity was split into multiple aliases"

    bind(after["canvas"], before["canvas"], "canvas")
    mapping[after["owner"]] = before["owner"]
    assert (before["name"], before["initial_name"]) == (after["name"], after["initial_name"])
    old_events, new_events = before["events"], after["events"]
    assert len(old_events) == len(new_events)
    credits = {actor["imported"]: actor["original"] for actor in origin["actors"]}
    prior_credits = {actor["imported"]: actor["original"] for actor in before.get("import_origin", {}).get("actors", [])}
    devices = {}
    for old, new in zip(old_events, new_events):
        old_op, new_op = old["op"], new["op"]
        for field in ("id", "author", "device"):
            bind(new_op[field], old_op[field], field)
        assert credits[new_op["author"]] == prior_credits.get(old_op["author"], old_op["author"])
        devices[old_op["device"]] = new_op["device"]
        old_action, new_action = old_op["action"], new_op["action"]
        assert old_action["kind"] == new_action["kind"]
        if "provenance" in old_action:
            assert old_action["provenance"] == new_action["provenance"], "Stroke provenance changed"
        if old_action["kind"] == "place_media":
            assert old_action["media"]["provenance"] == new_action["media"]["provenance"], "Media provenance changed"
        if old_action["kind"] == "put_tour":
            assert old_action["tour"]["name"] == new_action["tour"]["name"]
        if old_action["kind"] == "rename":
            assert old_action["name"] == new_action["name"]
        for field in ("stroke", "media", "tour"):
            if field in old_action:
                old_id, new_id = old_action[field], new_action[field]
                if isinstance(old_id, dict):
                    old_id, new_id = old_id["id"], new_id["id"]
                bind(new_id, old_id, field)
        # Historical grants never become destination membership grants.
        assert new_op["authority_epoch"] == 1
        old_op["authority_epoch"] = 1
    assert list(sorted(devices.values())) == [devices[key] for key in sorted(devices)], "Device layer order changed"

    def normalized(value, translate):
        if isinstance(value, dict):
            return {translate.get(key, key): normalized(item, translate) for key, item in value.items() if key != "import_origin"}
        if isinstance(value, list):
            return [normalized(item, translate) for item in value]
        if isinstance(value, str):
            return translate.get(value, value)
        return value

    assert normalized(after, mapping) == normalized(before, {}), "Retained geometry, history, tours or state changed beyond import identities"
    for frame in source["assets"]:
        name = frame["hash"] + ".png"
        original = (source_dir / name).read_bytes()
        imported = (imported_dir / name).read_bytes()
        assert original == imported
        assert hashlib.sha256(imported).hexdigest() == frame["hash"]
    return {"passed": True, "events": len(old_events), "artists": len(credits), "devices": len(devices),
            "strokes": len(before["strokes"]), "tours": len(before["tours"]), "assets": len(source["assets"]),
            "source_canvas": before["canvas"], "destination_canvas": after["canvas"]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("imported", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = verify(args.source, args.imported)
    args.output.write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result))
