#!/usr/bin/env python3
"""Regenerates crates/phaedrus_script_editor/data/roblox-api.json, the class
and enum data behind the script editor's intellisense.

It trims the Roblox API dump (as mirrored by MaximumADHD/Roblox-Client-Tracker)
down to what scripts can use: no deprecated or non-scriptable members
and nothing above normal script security.

Usage: python scripts/gen-roblox-api.py [path-or-url-to-API-Dump.json]
"""

import json
import os
import sys
import urllib.request

TRACKER = "https://raw.githubusercontent.com/MaximumADHD/Roblox-Client-Tracker/roblox/"
DEFAULT_URL = TRACKER + "Mini-API-Dump.json"
OUT = os.path.join(os.path.dirname(__file__), "..", "crates", "phaedrus_script_editor", "data", "roblox-api.json")

# "Hidden" only hides a member from the Properties panel (e.g. BasePart.Position).
SKIP_TAGS = {"Deprecated", "NotScriptable"}
PRIMITIVES = {
    "bool": "boolean",
    "int": "number",
    "int64": "number",
    "float": "number",
    "double": "number",
    "string": "string",
    "null": "nil",
    "void": "nil",
}


def tags(item):
    return {t for t in item.get("Tags", []) if isinstance(t, str)}


def type_name(t):
    if t is None:
        return "nil"
    if isinstance(t, list):  # multiple returns
        return "(" + ", ".join(type_name(x) for x in t) + ")"
    cat, name = t["Category"], t["Name"]
    if name.endswith("?"):
        inner = type_name({"Category": cat, "Name": name[:-1]})
        return inner if inner in ("any", "nil") else inner + "?"
    if cat == "Primitive":
        return PRIMITIVES.get(name, name)
    if cat == "Enum":
        return "Enum." + name
    if cat == "Group":
        return {"Array": "{any}", "Objects": "{Instance}", "Dictionary": "{[string]: any}",
                "Map": "{[string]: any}", "Tuple": "...any", "Variant": "any", "Function": "function"}.get(name, "any")
    if name in ("Objects", "Instances"):
        return "{Instance}"
    if name == "Object":
        return "any"
    if name in ("Content", "ContentId", "ProtectedString", "BinaryString", "SharedString"):
        return "string"
    if name == "Function":
        return "function"
    return name


def params(ps):
    out = []
    for p in ps:
        entry = [p["Name"], type_name(p["Type"])]
        if "Default" in p:
            entry.append(p["Default"])
        out.append(entry)
    return out


def main():
    src = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_URL
    if src.startswith("http"):
        with urllib.request.urlopen(src) as r:
            dump = json.load(r)
    else:
        with open(src, encoding="utf-8") as f:
            dump = json.load(f)

    classes = []
    for c in dump["Classes"]:
        members = []
        for m in c["Members"]:
            if tags(m) & SKIP_TAGS:
                continue
            kind = m["MemberType"]
            sec = m.get("Security")
            if kind == "Property":
                if sec["Read"] != "None":
                    continue
                ro = "ReadOnly" in tags(m) or sec["Write"] != "None"
                members.append(["p", m["Name"], type_name(m["ValueType"]), 1 if ro else 0])
            elif sec != "None":
                continue
            elif kind == "Function":
                members.append(["f", m["Name"], params(m["Parameters"]), type_name(m.get("ReturnType")),
                                1 if "Yields" in tags(m) else 0])
            elif kind == "Event":
                members.append(["e", m["Name"], params(m["Parameters"])])
            elif kind == "Callback":
                members.append(["c", m["Name"], params(m["Parameters"]), type_name(m.get("ReturnType"))])
        members.sort(key=lambda x: x[1])
        ctags = sorted(tags(c) & {"Service", "NotCreatable", "Deprecated", "NotBrowsable"})
        superclass = c["Superclass"] if c["Superclass"] != "<<<ROOT>>>" else None
        classes.append({"name": c["Name"], "superclass": superclass, "tags": ctags, "members": members})
    classes.sort(key=lambda c: c["name"])

    enums = {}
    for e in dump["Enums"]:
        if "Deprecated" in tags(e):
            continue
        enums[e["Name"]] = [i["Name"] for i in e["Items"] if "Deprecated" not in tags(i)]

    version = None
    if src == DEFAULT_URL:
        with urllib.request.urlopen(TRACKER + "version.txt") as r:
            version = r.read().decode().strip()
    out = {"version": version, "classes": classes, "enums": dict(sorted(enums.items()))}
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        json.dump(out, f, separators=(",", ":"))
        f.write("\n")
    print(f"wrote {len(classes)} classes and {len(enums)} enums to {os.path.normpath(OUT)}")


if __name__ == "__main__":
    main()
