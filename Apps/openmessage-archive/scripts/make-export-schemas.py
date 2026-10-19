#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Write the JSON export's schemas (M6 spec 5.1, slice 6f):

- crates/oma-export/schema/export-v1.strict.schema.json: the strict record
  schema (CI only, never shipped). `additionalProperties: false` on every
  object, and every code field restricted to this build's closed
  (token, code) pairs of its registry table, read from
  crates/oma-export/tests/codes.pin, plus `unrecognisedCode` with a number
  the table does not hold. A new field, a token our own code does not know
  or a read, delivery or sent key (D94) therefore fails validation.
- crates/oma-export/schema/export-v1.schema.json: the open record schema,
  published, a copy in every JSON bundle. The same structure, derived from
  the strict one mechanically: objects accept unknown keys and a code is
  any {token, code}, so consumers stay valid across MINOR versions.
- crates/oma-export/schema/manifest-v1.schema.json: manifest.json (open).

Each describes the records in crates/oma-export/src/record.rs (their serde
form) and the files of crates/oma-export/src/writer/json.rs. JSON Schema
2020-12; the `$id`s are URNs (nothing is fetched). Run from the app
directory with `python3 -I scripts/make-export-schemas.py` and commit the
result; `tests/json.rs` pins the strict schema's SHA-256 against
`schemaVersion`, so a schema change without a version bump fails, and
checks the open schema is still the strict one's derivation. Only the
standard library is used.
"""
import copy
import json
import pathlib
import sys

APP = pathlib.Path(__file__).resolve().parent.parent
CRATE = APP / "crates" / "oma-export"
OUT = CRATE / "schema"
DRAFT = "https://json-schema.org/draft/2020-12/schema"
SCHEMA_VERSION = "1.1.0"
FORMAT = "openmessage-archive-export"
UNRECOGNISED = "unrecognisedCode"


def registries():
    regs = {}
    for line in (CRATE / "tests" / "codes.pin").read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        name, _source, code, token = line.split("\t")
        regs.setdefault(name, []).append((token, int(code)))
    return regs


REGS = registries()
DEFS = {}


def ref(name):
    return {"$ref": f"#/$defs/{name}"}


def code(reg):
    if reg not in REGS:
        raise SystemExit(f"no registry {reg}")
    return ref(f"code-{reg}")


def nullable(s):
    return {"anyOf": [s, {"type": "null"}]}


def array(s):
    return {"type": "array", "items": s}


def const(v):
    return {"const": v}


STRING = {"type": "string"}
BOOL = {"type": "boolean"}
UINT = {"type": "integer", "minimum": 0}
INT = {"type": "integer"}
ORD = ref("ordinal")
DEC = ref("decimal")
CONTENT = ref("content")


def obj(props, optional=()):
    """An object whose every key is present (serde writes every field),
    except those in `optional`."""
    return {
        "type": "object",
        "required": [k for k in props if k not in optional],
        "properties": props,
        "additionalProperties": False,
    }


def tagged(tag, variants):
    """A serde internally tagged enum: one object per variant, the tag a
    const."""
    out = []
    for name, props in variants:
        out.append(obj({tag: const(name), **props}))
    return {"oneOf": out}


def define(name, s):
    DEFS[name] = s
    return ref(name)


def included(value, counts, options):
    return tagged("state", [
        ("notIncluded", {"option": {"enum": options}, "counts": counts}),
        ("included", {"value": value}),
    ])


# Scalars.
DEFS["ordinal"] = {"type": "integer", "minimum": 1,
                   "description": "An export-local ordinal (spec 4.9)."}
DEFS["decimal"] = {"type": "string", "pattern": "^-?[0-9]+$",
                   "description": "A raw integer or an archive id as a decimal string (RFC 7493 2.2)."}
DEFS["realDecimal"] = {"type": "string", "pattern": "^(-?[0-9]+(\\.[0-9]+)?(e-?[0-9]+)?|-?inf|NaN)$",
                       "description": "A real number as stored, in its shortest round-trip form."}
DEFS["content"] = {"type": "string", "description": "Content text, as recorded (never normalised)."}
DEFS["code"] = obj({"token": STRING, "code": INT})
for reg, entries in sorted(REGS.items()):
    known = [c for _, c in entries]
    DEFS[f"code-{reg}"] = {"oneOf": [obj({"token": const(t), "code": const(c)}) for t, c in entries]
                           + [obj({"token": const(UNRECOGNISED), "code": {"type": "integer", "not": {"enum": known}}})]}

NO_COUNTS = obj({})
OPTIONS = ["history", "reactions", "events", "identifiers"]

text_value = define("textValue", obj({
    "text": nullable(CONTENT), "missing": nullable(code("valueState")), "lossy": BOOL,
    "storedAsBlob": BOOL, "bidiControls": BOOL, "invisible": BOOL}))
raw_int = define("rawInt", obj({"raw": nullable(DEC), "state": nullable(code("valueState"))}))
state_bool = define("stateBool", obj({
    "value": nullable(BOOL), "state": nullable(code("valueState")), "derived": const(True)}))
raw_time = define("rawTime", obj({
    "raw": nullable(DEC), "state": nullable(code("valueState")), "unit": nullable(code("timeUnit")),
    "unitRule": const("magnitudeWindows")}))
time_rec = define("time", obj({
    "raw": nullable(DEC), "state": nullable(code("valueState")), "unit": nullable(code("timeUnit")),
    "unitRule": const("magnitudeWindows"), "utc": nullable(STRING), "local": nullable(STRING),
    "offsetSeconds": nullable(INT), "outsideZoneWindow": BOOL, "before2007": BOOL, "planned": BOOL}))
msg_ref = define("messageRef", tagged("scope", [("inExport", {"n": ORD}), ("notInExport", {})]))
conv_ref = define("conversationRef", tagged("scope", [
    ("inExport", {"n": ORD}), ("notInExport", {}), ("notRecorded", {})]))
recorded_by = define("recordedBy", tagged("kind", [
    ("singleLineage", {}), ("lineage", {"lineage": ORD, "session": ORD})]))
direction = define("direction", obj({
    "token": code("direction"), "state": nullable(code("valueState")), "raw": nullable(DEC),
    "recordedBy": recorded_by}))
raw_direction = define("rawDirection", obj({
    "token": code("direction"), "state": nullable(code("valueState")), "raw": nullable(DEC)}))
sender = define("sender", obj({"reason": code("senderReason"), "handle": nullable(ORD)}))
segment = define("segment", obj({
    "kind": code("segmentKind"), "text": nullable(CONTENT), "withheld": nullable(code("maskKind")),
    "attachment": nullable(ORD), "transcription": BOOL, "mention": BOOL, "url": BOOL,
    "urlHidden": BOOL, "oneTimeCode": BOOL, "styles": array(code("style")),
    "utf16Start": UINT, "utf16Len": UINT}))
body = define("body", tagged("kind", [
    ("text", {"segments": array(segment), "formatting": code("formatting"), "utf16Len": UINT,
              "bidiControls": BOOL, "invisible": BOOL, "lossy": BOOL, "storedAsBlob": BOOL}),
    ("noText", {"reason": code("noTextReason")})]))
body_facts = define("bodyFacts", obj({
    "bodyRow": BOOL, "displaySource": nullable(code("displaySource")),
    "displayNoneReason": nullable(code("displayNoneReason")),
    "attributedOutcome": nullable(code("attributedOutcome")),
    "attributedError": nullable(code("tsError")), "runIssue": nullable(code("tsError")),
    "textRawState": nullable(code("valueState")), "sourcesDiffer": nullable(BOOL)}))
piece = define("piece", obj({
    "kind": code("segmentKind"), "text": nullable(CONTENT), "withheld": nullable(code("maskKind"))}))
alt_text = define("alternativeText", tagged("state", [
    ("none", {}),
    ("text", {"source": code("altTextSource"), "text": text_value}),
    ("withheld", {"why": code("altTextWithheld"), "reason": nullable(code("withheldReason"))})]))
edit = define("edit", obj({
    "label": nullable(code("editLabel")), "editedParts": array(DEC), "unsentParts": array(DEC),
    "unsentWhole": BOOL, "partCount": nullable(UINT), "storedVersions": UINT}))
earlier = define("earlierText", tagged("state", [
    ("none", {}), ("available", {}),
    ("withheld", {"reason": code("withheldReason")}),
    ("revealed", {"reason": code("withheldReason"), "pieces": array(piece), "session": ORD})]))
history_counts = define("historyCounts", obj({"capturedVersions": UINT, "phoneEntries": UINT}))
label = define("label", obj({
    "kind": code("messageLabel"), "value": nullable(DEC), "lineage": nullable(ORD), "derived": BOOL}))
recently_deleted = define("recentlyDeleted", obj({
    "session": ORD, "conversation": conv_ref, "notListedLater": array(ORD), "deleted": raw_time,
    "ckSyncState": raw_int}))
association = define("association", obj({
    "determinable": BOOL, "kind": nullable(code("assocKind")), "shape": nullable(code("assocTargetShape")),
    "visibility": code("assocVisibility"), "target": nullable(msg_ref), "targetPart": nullable(DEC),
    "emoji": nullable(text_value)}))
reaction_row = define("reactionRow", obj({
    "n": ORD, "kind": code("assocKind"), "adds": BOOL, "emoji": nullable(text_value),
    "direction": raw_direction, "handle": nullable(ORD), "date": time_rec, "targetPart": nullable(DEC)}))
chip = define("chip", obj({
    "kind": code("reactionKind"), "emoji": nullable(text_value),
    "sender": obj({"kind": code("chipSender"), "handle": nullable(ORD)}),
    "state": code("chipState"), "rows": array(ORD), "includesRowsOutsideScope": BOOL, "derived": BOOL}))
reactions = define("reactions", obj({
    "rows": array(reaction_row), "chips": array(chip), "rule": const("d93LatestDatedRow"),
    "unpaired": array(msg_ref)}))
reply = define("reply", obj({"resolution": code("replyResolution"), "originator": nullable(msg_ref)}))
reply_count = define("replyCount", tagged("state", [
    ("notDeterminable", {"reason": {"enum": ["unkeyed"]}}),
    ("determined", {"value": obj({"inExport": UINT, "elsewhere": UINT})})]))
event = define("event", obj({
    "kind": code("itemKind"), "handles": array(ORD), "title": nullable(text_value)}))
value_history = define("valueHistory", tagged("state", [
    ("value", {"column": code("historyColumn"), "value": text_value, "firstSession": ORD}),
    ("withheld", {"column": code("historyColumn"), "reason": code("withheldReason")})]))
derivation = define("derivation", obj({"derivedBy": INT, "decoderVersion": nullable(INT)}))
raw_columns = define("rawColumns", obj({
    "service": text_value, **{k: raw_int for k in [
        "itemType", "groupActionType", "isAudioMessage", "cacheHasAttachments", "shareStatus",
        "shareDirection", "assocType", "assocRangeLocation", "assocRangeLength"]},
    "threadOriginatorPart": text_value, **{k: raw_int for k in [
        "partCount", "scheduleState", "filterAction", "filterSubAction", "wasDowngraded"]}}))
message_source = obj({
    "archiveId": DEC, "guid": text_value, "assocGuid": text_value,
    "threadOriginatorGuid": text_value, "replyToGuid": text_value})
context = define("context", obj({
    "conversation": nullable(ORD),
    "alsoIn": obj({"inExport": array(ORD), "elsewhere": UINT}),
    "foldedInto": nullable(ORD), "replyInConversation": nullable(BOOL),
    "targetInConversation": nullable(BOOL), "joinDate": nullable(raw_time), "orderTie": BOOL}))
observation = define("observation", obj({
    "session": ORD, "lookup": code("lookup"), "copy": code("copy"), "pathClass": nullable(code("pathClass")),
    "sizeMatchedManifest": nullable(BOOL), "holdsStoredFile": BOOL}))
card = define("card", obj({
    "state": code("cardState"), "olderBackupHeldBy": nullable(ORD), "olderBackupNewest": nullable(ORD),
    "locationLookup": nullable(code("lookup")), "unavailableLookup": nullable(DEC),
    "unavailableCopy": nullable(DEC)}))
attachment = define("attachment", obj({
    "n": ORD,
    "source": included(obj({"archiveId": DEC, "guid": text_value, "originalGuid": text_value}),
                       NO_COUNTS, ["identifiers"]),
    "file": obj({"outcome": code("attachmentOutcome"), "file": nullable(ORD), "path": nullable(CONTENT),
                 "sniffed": nullable(code("sniffedKind"))}),
    "access": code("attachmentAccess"), "accessReason": nullable(code("withheldReason")),
    "name": text_value, "mimeType": text_value, "uti": text_value, "totalBytes": raw_int,
    "storedBytes": nullable(DEC), "isSticker": raw_int, "hideAttachment": raw_int, "isOutgoing": raw_int,
    "transferState": raw_int, "emojiDescription": text_value, "created": raw_time, "start": raw_time,
    "placedInline": BOOL, "card": card, "heldBy": nullable(ORD), "observations": array(observation),
    "verify": nullable(obj({"state": code("blobVerifyState"), "mayBeStale": BOOL})),
    "history": array(value_history)}))

item_props = {
    "n": ORD,
    "source": included(message_source, NO_COUNTS, ["identifiers"]),
    "item": code("itemKind"), "direction": direction, "sender": sender,
    "service": nullable(code("serviceClass")), "date": time_rec, "dateEdited": raw_time,
    "dateRetracted": raw_time, "subject": text_value, "account": text_value, "body": body,
    "alternativeText": alt_text, "bodyFacts": body_facts, "edit": edit, "earlierText": earlier,
    "history": included(history_counts, history_counts, ["history"]),
    "labels": array(label), "recentlyDeleted": array(recently_deleted),
    "association": nullable(association),
    "reactions": included(reactions, obj({"rows": UINT}), ["reactions"]),
    "reply": nullable(reply), "replyCount": reply_count, "event": nullable(event),
    "attachments": array(attachment), "scheduleType": raw_int, "appContent": state_bool,
    "effect": state_bool, "valueHistory": array(value_history), "raw": raw_columns,
    "derivation": derivation, "context": context,
}
DEFS["item"] = obj({"type": const("item"), **item_props})

edit_entry = define("editEntry", obj({
    "part": DEC, "ordinal": DEC,
    "at": obj({"kind": nullable(code("summaryTimeKind")), "rawType": nullable({"enum": ["integer", "real"]}),
               "raw": nullable({"anyOf": [DEC, ref("realDecimal")]})}),
    "text": tagged("state", [
        ("text", {"pieces": array(piece)}), ("withheld", {"reason": code("withheldReason")}),
        ("noBody", {}), ("failed", {"decodeError": code("tsError")}), ("notMatched", {})])}))
DEFS["version"] = obj({
    "type": const("version"), "message": ORD, "current": BOOL, "firstSession": ORD, "lastSession": ORD,
    "text": tagged("state", [
        ("text", {"pieces": array(piece)}), ("withheld", {"reason": code("withheldReason")}),
        ("noText", {"reason": code("noTextReason"),
                    "displayNoneReason": nullable(code("displayNoneReason"))})]),
    "textUnchanged": BOOL, "editLabel": nullable(code("editLabel")), "edits": array(edit_entry),
    "editTexts": code("editTextsOutcome")})

conversation = define("conversation", obj({
    "n": ORD,
    "source": included(obj({"archiveId": DEC, "guid": text_value, "roomName": text_value,
                            "groupId": text_value}), NO_COUNTS, ["identifiers"]),
    "name": text_value, "chatIdentifier": text_value, "serviceName": text_value,
    "service": nullable(code("serviceClass")), "style": raw_int, "isFiltered": raw_int,
    "isArchived": raw_int, "isBlackholed": raw_int, "isPendingReview": raw_int, "isRecovered": raw_int,
    "propertiesBytes": raw_int, "participants": array(ORD), "nameHistory": array(value_history)}))
handle = define("handle", obj({
    "n": ORD,
    "source": included(obj({"archiveId": DEC, "uncanonicalizedId": text_value,
                            "personCentricId": text_value}), NO_COUNTS, ["identifiers"]),
    "address": text_value, "service": text_value, "serviceClass": nullable(code("serviceClass")),
    "country": text_value}))

IDENT = {"format": const(FORMAT), "schemaVersion": const(SCHEMA_VERSION)}
DEFS["header"] = obj({"type": const("header"), **IDENT, "conversation": nullable(ORD)})
DEFS["conversationsFile"] = obj({"type": const("conversations"), **IDENT,
                                 "conversations": array(conversation)})
DEFS["handlesFile"] = obj({"type": const("handles"), **IDENT, "handles": array(handle)})
DEFS["line"] = {"oneOf": [ref("header"), ref("item"), ref("version")],
                "description": "One line of a c<nnnn>.jsonl or no-conversation.jsonl file."}
DEFS["singleFile"] = obj({
    **IDENT, "conversation": nullable(conversation),
    "items": array({"oneOf": [ref("item"), ref("version")]}),
    "handles": array(handle),
    "about": {"type": "object", "description": "The manifest's fields without `files` "
                                               "(urn:openmessage-archive:export-manifest:1)."}})


def strict_schema():
    return {
        "$schema": DRAFT,
        "$id": f"urn:openmessage-archive:export:{SCHEMA_VERSION}:strict",
        "title": "OpenMessage Archive export records (strict, CI only)",
        "description": "Every JSON document of an export: a JSON Lines line (header, item or version), "
                       "json/conversations.json, json/handles.json, or a single-file export. Strict: no "
                       "unknown key, and only this build's (token, code) pairs or unrecognisedCode.",
        "oneOf": [ref(n) for n in ("header", "item", "version", "conversationsFile", "handlesFile",
                                   "singleFile")],
        "$defs": DEFS,
    }


def opened(s):
    """The open form: unknown keys allowed, codes open."""
    if isinstance(s, dict):
        out = {k: opened(v) for k, v in s.items() if not (k == "additionalProperties" and v is False)}
        return out
    if isinstance(s, list):
        return [opened(v) for v in s]
    return s


def open_schema(strict):
    s = opened(copy.deepcopy(strict))
    s["$id"] = "urn:openmessage-archive:export:1"
    s["title"] = "OpenMessage Archive export records"
    s["description"] = ("Every JSON document of an export: a JSON Lines line (header, item or version), "
                        "json/conversations.json, json/handles.json, or a single-file export. Open: "
                        "consumers must accept unknown keys and unknown code tokens (every token travels "
                        "with its numeric code).")
    for name in list(s["$defs"]):
        if name.startswith("code-"):
            s["$defs"][name] = {"$ref": "#/$defs/code", "description": f"A `{name[5:]}` code."}
    return s


def manifest_schema():
    counts_keys = [
        "conversations", "timelineRows", "distinctMessages", "crossConversationRows", "foldedReactionRows",
        "items", "undatedExcluded", "before2007", "noText", "lossy", "orderTies", "eventsNotIncluded",
        "oneTimeCodesMasked", "bodiesWithheldWhole", "earlierTextWithheld", "editTextWithheld",
        "alternativeTextWithheld", "hiddenCharactersMarked", "controlsEscaped", "combiningRunsLimited",
        "neutralisedCells", "continuationRows", "pdfUndrawableCharacters", "pdfForcedWraps",
        "timesOutsideZoneWindow"]
    att_keys = ["records", "withFile", "notRequested", "filesWritten", "deduplicated", "blocked",
                "namesShortened", "previewsEmbedded", "previewsUnavailable",
                "previewsSkippedAfterHelperFailures", "motwNotWritten"]
    count_map = {"type": "object", "additionalProperties": UINT}

    def o(props, optional=()):
        return {"type": "object", "required": [k for k in props if k not in optional], "properties": props}

    date = {"type": "string", "pattern": "^-?[0-9]{4,}-[0-9]{2}-[0-9]{2}$"}
    iso = {"type": "string", "pattern": "^[-+]?[0-9]{4,}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"}
    body = {
        "format": const("openmessage-archive-export-manifest"),
        "manifestVersion": const("1.0.0"), "schemaVersion": const(SCHEMA_VERSION),
        "tabularColumnsVersion": STRING, "appVersion": STRING, "archiveSchemaVersion": UINT,
        "exportId": {"type": "string", "pattern": "^[0-9a-f]{32}$"}, "createdUtc": iso,
        "snapshot": o({"kind": STRING, "archiveLockHeld": BOOL}),
        "scope": o({"kind": {"enum": ["conversation", "dateRange", "searchResults", "archive"]},
                    "fromLocalDate": date, "untilLocalDate": date, "fromUtc": iso, "untilUtc": iso,
                    "conversations": UINT},
                   optional=("fromLocalDate", "untilLocalDate", "fromUtc", "untilUtc")),
        "formats": array({"enum": ["txt", "csv", "json", "html", "pdf", "xlsx"]}),
        "options": o({
            "attachments": {"enum": ["namesOnly", "copyOriginals"]}, "keepExtensions": BOOL,
            "previews": BOOL, "largerPreviews": BOOL, "history": BOOL, "reactions": BOOL, "events": BOOL,
            "reveal": o({"oneTimeCodes": BOOL, "earlierContent": BOOL,
                          "oneTimeCodesConfirmedUpTo": nullable(UINT),
                          "earlierContentConfirmedUpTo": nullable(UINT)}),
            "clock": {"enum": ["h23", "h12"]}, "seconds": BOOL, "lineEndings": {"enum": ["lf", "crlf"]},
            "hiddenCharacterMarkers": BOOL, "csvProfile": nullable({"enum": ["spreadsheet", "dataTools"]}),
            "identifiers": {"enum": ["ordinals", "archiveAndSource"]},
            "pageSize": nullable({"enum": ["a4", "letter"]})}),
        "zone": o({"name": {"type": "string", "pattern": "^[A-Za-z0-9_+\\-/]{1,64}$"},
                   "source": {"enum": ["utc", "intl"]},
                   "windowUtc": nullable({"type": "array", "prefixItems": [iso, iso], "items": False}),
                   "transitions": array({"type": "array", "prefixItems": [iso, INT], "items": False})}),
        "derivation": o({"derivedBy": count_map, "decoderVersion": count_map}),
        "counts": o({**{k: UINT for k in counts_keys},
                     "attachments": o({**{k: UINT for k in att_keys}, "excluded": count_map})}),
        "search": nullable(o({
            "mode": STRING, "filterKinds": array(STRING), "includeEarlierForMatching": BOOL,
            "matchesWritten": UINT, "mayBeIncomplete": BOOL, "allMatches": BOOL})),
        "importSessions": array(o({"session": ORD, "backupDate": nullable(date), "dateSource": STRING,
                                   "completed": BOOL})),
        "exclusions": array(STRING),
    }
    return {
        "$schema": DRAFT,
        "$id": "urn:openmessage-archive:export-manifest:1",
        "title": "OpenMessage Archive export manifest",
        "description": "manifest.json of an export bundle (spec 7.1). It never holds a search query, a "
                       "device fact, an archive id or label, a GUID or a path outside the bundle.",
        "type": "object",
        "required": list(body) + ["files"],
        "properties": {**body, "files": array(o({
            "path": {"type": "string", "pattern": "^[^/\\\\]+(/[^/\\\\]+)*$"}, "bytes": UINT,
            "sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"}}))},
        "$defs": {"ordinal": DEFS["ordinal"]},
    }


def write(name, doc):
    text = json.dumps(doc, indent=2, ensure_ascii=True) + "\n"
    (OUT / name).write_text(text, encoding="ascii", newline="\n")
    return len(text)


def main():
    OUT.mkdir(exist_ok=True)
    strict = strict_schema()
    n = write("export-v1.strict.schema.json", strict)
    m = write("export-v1.schema.json", open_schema(strict))
    k = write("manifest-v1.schema.json", manifest_schema())
    print(f"wrote 3 schemas ({n}, {m}, {k} bytes); {len(REGS)} code tables")
    return 0


if __name__ == "__main__":
    sys.exit(main())
