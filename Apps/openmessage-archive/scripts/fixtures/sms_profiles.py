# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Column sets for the SYNTHETIC sms.db stand-ins of OpenMessage Archive.

Hand-written from column-name facts only: the known-column list of
crates/oma-messages (M3 spec section 6.1, sources S2, S3, S5, S8, S12 and
S13 by name). No DDL text was copied from any sample database, Apple's or a
third party's; the declared types, the order and the indexes here are our
own choices. Apple's triggers are omitted, because they call functions that
exist only on Apple platforms.

The profiles have neutral names. Snapshots A (S12) and C (S3) are both macOS
chat.db, so no profile is an iOS version, and none is claimed to match one.

KNOWN is a second statement of `schema/known.rs`: the generator's report
model uses it to decide which known columns are missing, how each value is
read, and each column's byte cap.
"""

KIB = 1024
MIB = 1024 * KIB
CAP_ATTRIBUTED_BODY = 16 * MIB
CAP_TEXT = 16 * MIB
CAP_SUMMARY = MIB
CAP_PROPERTIES = 256 * KIB
CAP_GUID = 256
CAP_IDENTIFIER = 4 * KIB
CAP_OTHER_TEXT = 64 * KIB

# kind: rowid | int | bool | time | text | blob | blobsize | typeonly
# (name, kind, cap, required)
KNOWN = {
    "message": [
        ("ROWID", "rowid", 0, True),
        ("guid", "text", CAP_GUID, True),
        ("date", "time", 0, True),
        ("is_from_me", "bool", 0, True),
        ("text", "text", CAP_TEXT, False),
        ("attributedBody", "blob", CAP_ATTRIBUTED_BODY, False),
        ("service", "text", CAP_OTHER_TEXT, False),
        ("handle_id", "int", 0, False),
        ("other_handle", "int", 0, False),
        ("destination_caller_id", "text", CAP_IDENTIFIER, False),
        ("subject", "text", CAP_OTHER_TEXT, False),
        ("date_read", "time", 0, False),
        ("date_delivered", "time", 0, False),
        ("date_played", "time", 0, False),
        ("date_edited", "time", 0, False),
        ("date_retracted", "time", 0, False),
        ("is_read", "bool", 0, False),
        ("is_delivered", "bool", 0, False),
        ("is_sent", "bool", 0, False),
        ("is_audio_message", "bool", 0, False),
        ("cache_has_attachments", "bool", 0, False),
        ("item_type", "int", 0, False),
        ("group_action_type", "int", 0, False),
        ("group_title", "text", CAP_OTHER_TEXT, False),
        ("share_status", "int", 0, False),
        ("share_direction", "bool", 0, False),
        ("associated_message_guid", "text", CAP_GUID, False),
        ("associated_message_type", "int", 0, False),
        ("associated_message_emoji", "text", CAP_OTHER_TEXT, False),
        ("associated_message_range_location", "int", 0, False),
        ("associated_message_range_length", "int", 0, False),
        ("balloon_bundle_id", "text", CAP_OTHER_TEXT, False),
        ("expressive_send_style_id", "text", CAP_OTHER_TEXT, False),
        ("payload_data", "typeonly", 0, False),
        ("message_summary_info", "blob", CAP_SUMMARY, False),
        ("thread_originator_guid", "text", CAP_GUID, False),
        ("thread_originator_part", "text", CAP_OTHER_TEXT, False),
        ("reply_to_guid", "text", CAP_GUID, False),
        ("part_count", "int", 0, False),
        ("schedule_type", "int", 0, False),
        ("schedule_state", "int", 0, False),
        ("filter_action", "int", 0, False),
        ("filter_sub_action", "int", 0, False),
        ("error", "int", 0, False),
        ("was_downgraded", "bool", 0, False),
    ],
    "handle": [
        ("ROWID", "rowid", 0, True),
        ("id", "text", CAP_IDENTIFIER, True),
        ("service", "text", CAP_OTHER_TEXT, False),
        ("country", "text", CAP_OTHER_TEXT, False),
        ("uncanonicalized_id", "text", CAP_IDENTIFIER, False),
        ("person_centric_id", "text", CAP_IDENTIFIER, False),
    ],
    "chat": [
        ("ROWID", "rowid", 0, True),
        ("guid", "text", CAP_GUID, True),
        ("style", "int", 0, False),
        ("chat_identifier", "text", CAP_IDENTIFIER, False),
        ("service_name", "text", CAP_OTHER_TEXT, False),
        ("display_name", "text", CAP_OTHER_TEXT, False),
        ("room_name", "text", CAP_OTHER_TEXT, False),
        ("group_id", "text", CAP_IDENTIFIER, False),
        ("properties", "blobsize", CAP_PROPERTIES, False),
        ("is_archived", "bool", 0, False),
        ("is_filtered", "int", 0, False),
        ("is_blackholed", "bool", 0, False),
        ("is_pending_review", "bool", 0, False),
        ("is_recovered", "bool", 0, False),
        ("last_read_message_timestamp", "time", 0, False),
    ],
    "chat_message_join": [
        ("chat_id", "int", 0, True),
        ("message_id", "int", 0, True),
        ("message_date", "time", 0, False),
    ],
    "chat_handle_join": [
        ("chat_id", "int", 0, True),
        ("handle_id", "int", 0, True),
    ],
    "message_attachment_join": [
        ("message_id", "int", 0, True),
        ("attachment_id", "int", 0, True),
    ],
    "attachment": [
        ("ROWID", "rowid", 0, True),
        ("guid", "text", CAP_GUID, True),
        ("filename", "text", CAP_IDENTIFIER, False),
        ("uti", "text", CAP_OTHER_TEXT, False),
        ("mime_type", "text", CAP_OTHER_TEXT, False),
        ("transfer_name", "text", CAP_IDENTIFIER, False),
        ("total_bytes", "int", 0, False),
        ("is_sticker", "bool", 0, False),
        ("hide_attachment", "bool", 0, False),
        ("is_outgoing", "bool", 0, False),
        ("transfer_state", "int", 0, False),
        ("created_date", "time", 0, False),
        ("start_date", "time", 0, False),
        ("emoji_image_short_description", "text", CAP_OTHER_TEXT, False),
        ("original_guid", "text", CAP_GUID, False),
    ],
    "chat_recoverable_message_join": [
        ("chat_id", "int", 0, True),
        ("message_id", "int", 0, True),
        ("delete_date", "time", 0, False),
        ("ck_sync_state", "int", 0, False),
    ],
    "recoverable_message_part": [],
    "deleted_messages": [],
    "sync_deleted_messages": [],
    "sync_deleted_chats": [],
    "sync_deleted_attachments": [],
    "unsynced_removed_recoverable_messages": [],
    "_SqliteDatabaseProperties": [
        ("key", "text", CAP_OTHER_TEXT, False),
        ("value", "text", CAP_OTHER_TEXT, False),
    ],
}

# The order of KnownTable::ALL.
TABLE_ORDER = list(KNOWN)
REQUIRED_TABLES = ["message", "handle", "chat", "chat_message_join", "chat_handle_join"]
ROWID_TABLES = ["message", "handle", "chat", "attachment"]
JOIN_TABLES = ["chat_message_join", "chat_handle_join", "message_attachment_join",
               "chat_recoverable_message_join"]
COUNT_ONLY = ["recoverable_message_part", "deleted_messages", "sync_deleted_messages",
              "sync_deleted_chats", "sync_deleted_attachments",
              "unsynced_removed_recoverable_messages"]


def known(table, column):
    for c in KNOWN.get(table, []):
        if c[0] == column:
            return c
    return None


DECL = {"rowid": "INTEGER PRIMARY KEY AUTOINCREMENT", "int": "INTEGER", "bool": "INTEGER DEFAULT 0",
        "time": "INTEGER DEFAULT 0", "text": "TEXT", "blob": "BLOB", "blobsize": "BLOB",
        "typeonly": "BLOB"}

# Columns of the count-only tables: ours, invented for the stand-in. Only
# count(*) is ever read from them, and recoverable_message_part's part_text is
# never read at all (the authorizer denies it).
COUNT_ONLY_DDL = {
    "recoverable_message_part": "chat_id INTEGER, message_id INTEGER, part_index INTEGER, "
                                "delete_date INTEGER, part_text BLOB",
    "deleted_messages": "ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT NOT NULL",
    "sync_deleted_messages": "ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT NOT NULL, recordID TEXT",
    "sync_deleted_chats": "ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT NOT NULL, recordID TEXT, timestamp INTEGER",
    "sync_deleted_attachments": "ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT NOT NULL, recordID TEXT",
    "unsynced_removed_recoverable_messages": "ROWID INTEGER PRIMARY KEY AUTOINCREMENT, chat_guid TEXT, "
                                             "message_guid TEXT, part_index INTEGER",
}

# ---- Column sets ----------------------------------------------------------

_A_MESSAGE = ["ROWID", "guid", "text", "handle_id", "service", "subject", "date",
              "date_read", "date_delivered", "is_from_me", "is_read", "is_delivered",
              "is_sent", "is_audio_message", "cache_has_attachments", "item_type",
              "group_action_type", "group_title", "other_handle", "share_status",
              "share_direction", "associated_message_guid", "associated_message_type",
              "balloon_bundle_id", "expressive_send_style_id", "payload_data",
              "attributedBody", "destination_caller_id", "error", "was_downgraded",
              "date_played"]
_A_CHAT = ["ROWID", "guid", "style", "chat_identifier", "service_name", "room_name",
           "display_name", "group_id", "properties", "is_archived", "is_filtered",
           "last_read_message_timestamp"]
_A_ATTACHMENT = ["ROWID", "guid", "created_date", "start_date", "filename", "uti",
                 "mime_type", "transfer_state", "is_outgoing", "transfer_name",
                 "total_bytes", "is_sticker", "hide_attachment", "original_guid"]
_HANDLE = ["ROWID", "id", "country", "service", "uncanonicalized_id", "person_centric_id"]
_THREADS_MESSAGE = ["thread_originator_guid", "thread_originator_part", "reply_to_guid",
                    "date_edited", "date_retracted", "part_count", "message_summary_info",
                    "associated_message_range_location", "associated_message_range_length"]
_RECOVERABLE = ["chat_recoverable_message_join", "recoverable_message_part",
                "deleted_messages", "sync_deleted_messages", "sync_deleted_chats",
                "sync_deleted_attachments", "unsynced_removed_recoverable_messages"]


def _profile(message, chat, attachment, cmj, recoverable, properties=True, extra=None):
    p = {
        "handle": list(_HANDLE),
        "message": list(message),
        "chat": list(chat),
        "chat_message_join": list(cmj),
        "chat_handle_join": ["chat_id", "handle_id"],
        "message_attachment_join": ["message_id", "attachment_id"],
        "attachment": list(attachment),
    }
    if recoverable:
        p["chat_recoverable_message_join"] = ["chat_id", "message_id", "delete_date",
                                              "ck_sync_state"]
        for t in _RECOVERABLE[1:]:
            p[t] = None  # count-only DDL
    if properties:
        p["_SqliteDatabaseProperties"] = ["key", "value"]
    for t, cols in (extra or {}).items():
        p[t] = p.get(t, []) + cols
    return p


def cols_a_seconds():
    """Snapshot-A column set (by name); no chat_message_join.message_date."""
    return _profile(_A_MESSAGE, _A_CHAT, _A_ATTACHMENT, ["chat_id", "message_id"], False)


def cols_threads_edits():
    return _profile(_A_MESSAGE + _THREADS_MESSAGE, _A_CHAT + ["is_blackholed", "is_recovered"],
                    _A_ATTACHMENT, ["chat_id", "message_id", "message_date"], True)


def cols_emoji_schedule():
    return _profile(_A_MESSAGE + _THREADS_MESSAGE + ["associated_message_emoji",
                                                     "schedule_type", "schedule_state"],
                    _A_CHAT + ["is_blackholed", "is_recovered"],
                    _A_ATTACHMENT + ["emoji_image_short_description"],
                    ["chat_id", "message_id", "message_date"], True)


def cols_filter():
    return _profile(_A_MESSAGE + ["filter_action", "filter_sub_action"],
                    _A_CHAT + ["is_pending_review"], _A_ATTACHMENT,
                    ["chat_id", "message_id"], False)


def full():
    """Every known column, plus one unknown column in message (a synthetic
    name), so the profile exercises the UnknownColumns qualifier."""
    p = {}
    for t, cols in KNOWN.items():
        if t in COUNT_ONLY:
            p[t] = None
        else:
            p[t] = [c[0] for c in cols]
    p["message"].append("synth_unknown_column")
    return p


def minimal():
    """The required tables with their required columns only."""
    return {t: [c[0] for c in KNOWN[t] if c[3]] for t in REQUIRED_TABLES}


def column_decl(table, col):
    k = known(table, col)
    if k is None:
        return "INTEGER"
    return DECL[k[1]]


# Our own indexes (not Apple's): enough that the probe sees some.
INDEXES = [
    ("message", "synth_message_idx_handle", "handle_id"),
    ("chat_message_join", "synth_cmj_idx_message", "message_id"),
    ("message_attachment_join", "synth_maj_idx_attachment", "attachment_id"),
]
