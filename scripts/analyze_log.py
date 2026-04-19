#!/usr/bin/env python3
"""
Scan an AI decision log (logs/*.jsonl) for taunt violations and AoO incidents.

A taunt violation: actor has a taunter (enemy with FORCES_TARGETING) but
commits a Cast at a non-taunter target. Fix 1 (filter in plan generator)
eliminates this class.

An AoO incident: committed path transitions out of adjacency with a melee
enemy (max_attack_range == 1) that has reactions available. Fix 2 adds
AoO-awareness to plan scoring.

Usage:
    scripts/analyze_log.py logs/<file>.jsonl
    scripts/analyze_log.py logs/*.jsonl
"""
from __future__ import annotations
import json
import sys
from pathlib import Path

FORCES_TARGETING_BIT = 0b0010_0000  # AiTags::FORCES_TARGETING (src/combat/ai/snapshot.rs)


def to_cube(h):
    col, row = h
    x = col - (row - (row & 1)) // 2
    z = row
    return (x, -x - z, z)


def hexdist(a, b):
    ax, ay, az = to_cube(a)
    bx, by, bz = to_cube(b)
    return max(abs(ax - bx), abs(ay - by), abs(az - bz))


def analyze_entry(d):
    snap = d["snapshot"]
    actor = next(u for u in snap["units"] if u["entity"] == d["actor_id"])
    team = actor["team"]
    enemies_alive = [u for u in snap["units"] if u["team"] != team and u["hp"] > 0]
    taunters = [e["entity"] for e in enemies_alive if e.get("tags", 0) & FORCES_TARGETING_BIT]
    melees = [(e["entity"], tuple(e["pos"])) for e in enemies_alive
              if e.get("max_attack_range", 0) == 1]

    cd = d["committed_decision"]
    path = cd.get("path") or []
    start = tuple(actor["pos"])
    target = cd.get("target_id")
    ability = cd.get("ability")

    taunt_violated = bool(
        taunters and target is not None and target not in taunters and ability
    )

    aoo_hits = []
    prev = start
    for step in path:
        st = tuple(step)
        for eid, epos in melees:
            if hexdist(prev, epos) == 1 and hexdist(st, epos) != 1:
                aoo_hits.append((eid, epos, prev, st))
        prev = st

    return {
        "round": d["round"],
        "actor_name": d.get("actor_name", "?"),
        "actor_hp": f"{actor['hp']}/{actor['max_hp']}",
        "start": start,
        "path": path,
        "kind": cd.get("kind", "?"),
        "target": target,
        "selection_kind": d["intent"]["selection_kind"],
        "taunters": taunters,
        "taunt_violated": taunt_violated,
        "aoo": aoo_hits,
    }


def report(path: Path):
    print(f"=== {path}")
    taunt_count = 0
    aoo_count = 0
    for line in path.open():
        e = analyze_entry(json.loads(line))
        markers = []
        if e["taunt_violated"]:
            markers.append("TAUNT_VIOL")
            taunt_count += 1
        if e["aoo"]:
            markers.append(f"AoO×{len(e['aoo'])}")
            aoo_count += 1
        marker = " " + " ".join(markers) if markers else ""
        name = e["actor_name"][:22]
        tinfo = f" taunters={e['taunters']}" if e["taunters"] else ""
        print(
            f"  r{e['round']} {name:22s} hp={e['actor_hp']:>6s} "
            f"{e['start']}→{e['path']} kind={e['kind']} "
            f"tgt={e['target']} sel={e['selection_kind']}{tinfo}{marker}"
        )
        for eid, epos, a, b in e["aoo"]:
            print(f"       AoO by {eid} at {epos}: {a}→{b}")
    print(f"  SUMMARY: {taunt_count} taunt violations, {aoo_count} AoO incidents\n")
    return taunt_count, aoo_count


def main(argv):
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    totals = (0, 0)
    for p in argv[1:]:
        t, a = report(Path(p))
        totals = (totals[0] + t, totals[1] + a)
    if len(argv) > 2:
        print(f"=== ALL: {totals[0]} taunt violations, {totals[1]} AoO incidents")


if __name__ == "__main__":
    main(sys.argv)
