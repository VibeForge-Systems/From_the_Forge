-- archive.sqlite, schema version 1.
-- Conventions (spec section 6.1):
--   x, x_st   x holds the value when x_st IS NULL. Otherwise x IS NULL and x_st says why:
--             1 = SQL NULL in the source, 2 = column absent in the source,
--             3 = withheld: over the read cap (byte count in value_issue),
--             4 = unreadable (IssueKind and, when kept, the raw value in value_issue).
--   *_raw/_st/_unit/_utc_ns   a Cocoa timestamp: the raw integer, its state, the per-value
--             unit (0 zero, 1 negative, 2 seconds, 3 nanoseconds, 4 ambiguous) and the
--             derived instant in Unix ns (only for units 2 and 3).
--   Text from sms.db is stored as BLOB: the UTF-8 bytes SQLite returned, unrepaired.
--   Only derived columns are TEXT.
--   *_rank    41-byte observation rank (spec 8.7): order class, order key, content digest;
--             compared with memcmp. *_session_id is the session that set the matching rank.
-- Every table is STRICT. Source ROWIDs are never keys outside one session.

CREATE TABLE archive_meta (
  id                   INTEGER PRIMARY KEY CHECK (id = 1),
  format               TEXT    NOT NULL CHECK (format = 'openmessage-archive'),
  schema_version       INTEGER NOT NULL,           -- mirrors PRAGMA user_version (sqlcipher_export drops it)
  archive_uuid         BLOB    NOT NULL CHECK (length(archive_uuid) = 16),
  created_at_ns        INTEGER NOT NULL,           -- host clock, Unix ns
  created_by           TEXT    NOT NULL,           -- app version that created the archive
  label                TEXT    CHECK (label IS NULL OR length(CAST(label AS BLOB)) <= 200), -- user-typed; never logged
  name_scheme          INTEGER NOT NULL CHECK (name_scheme = 1),  -- 1: blob storage_name = lowercase hex SHA-256
  max_deriver_version  INTEGER NOT NULL,           -- highest derivation-rule version that ever wrote rows
  max_decoder_version  INTEGER NOT NULL,           -- highest typedstream decoder version that ever wrote rows
  fts_tokenizer        TEXT    NOT NULL            -- recorded copy of the FTS tokenizer arguments
) STRICT;

CREATE TABLE schema_migration (
  version        INTEGER PRIMARY KEY,
  from_version   INTEGER NOT NULL,
  applied_at_ns  INTEGER NOT NULL,
  app_version    TEXT    NOT NULL
) STRICT;

-- One row per distinct backup observed. No UDID, serial, phone number or path (D14).
-- The strings are the plist values exactly as recorded: not sanitised, not truncated.
-- A value that is absent, of the wrong type or over 4096 bytes is NULL; the two field
-- bitmasks say which (bits: 1 device_name, 2 product_type, 4 product_version,
-- 8 build_version, 16 snapshot_state_raw, 32 info_last_backup_ns, 64 status_date_ns,
-- 128 is_full_backup). NULL with neither bit set means absent.
CREATE TABLE source_backup (
  id                     INTEGER PRIMARY KEY,
  observed_key           BLOB    NOT NULL UNIQUE CHECK (length(observed_key) = 32),
                                                   -- SHA-256 over the tagged encoding of the fields below
  device_name            TEXT,
  product_type           TEXT,
  product_version        TEXT,
  build_version          TEXT,
  info_last_backup_ns    INTEGER,                  -- Info.plist "Last Backup Date", Unix ns
  status_date_ns         INTEGER,                  -- Status.plist "Date", Unix ns
  snapshot_state_raw     TEXT,
  is_full_backup         INTEGER CHECK (is_full_backup IN (0, 1)),
  encrypted              INTEGER NOT NULL CHECK (encrypted IN (0, 1)),
  wrong_type_fields      INTEGER NOT NULL DEFAULT 0 CHECK (wrong_type_fields BETWEEN 0 AND 255),
  oversized_fields       INTEGER NOT NULL DEFAULT 0 CHECK (oversized_fields BETWEEN 0 AND 31)
) STRICT;

-- "Newer backups of the same phone" share a lineage; chosen by the user at the gate (D80).
CREATE TABLE lineage (
  id             INTEGER PRIMARY KEY,
  created_at_ns  INTEGER NOT NULL
) STRICT;

CREATE TABLE import_session (
  id                       INTEGER PRIMARY KEY,
  source_backup_id         INTEGER NOT NULL REFERENCES source_backup(id),
  lineage_id               INTEGER NOT NULL REFERENCES lineage(id),
  state                    INTEGER NOT NULL CHECK (state BETWEEN 0 AND 6),
                           -- 0 running, 1 completed, 2 cancelled, 3 failed, 4 interrupted,
                           -- 5 superseded, 6 stopped (resumable; reason in import_issue)
  started_at_ns            INTEGER NOT NULL,
  ended_at_ns              INTEGER,
  app_version              TEXT    NOT NULL,
  schema_version           INTEGER NOT NULL,
  deriver_version          INTEGER NOT NULL,
  decoder_version          INTEGER NOT NULL,
  read_profile             BLOB    NOT NULL CHECK (length(read_profile) = 32), -- SHA-256 of limits, caps, TsBudget
  content_digest           BLOB    NOT NULL CHECK (length(content_digest) = 32),
  main_len                 INTEGER NOT NULL,
  wal_outcome              INTEGER NOT NULL,         -- closed WalOutcome code
  wal_len                  INTEGER,                  -- NULL unless the -wal was staged
  source_text_encoding     INTEGER,                  -- closed TextEncoding code; NULL when not reported
  order_class              INTEGER NOT NULL CHECK (order_class IN (0, 1)), -- 0 undated, 1 dated
  order_key                INTEGER,                  -- Unix ns of the chosen backup date
  order_key_source         INTEGER CHECK (order_key_source IN (1, 2)),  -- 1 Status.plist Date, 2 Info.plist Last Backup Date
  rank                     BLOB    NOT NULL CHECK (length(rank) = 41),
  qualifiers               INTEGER NOT NULL,         -- bitmask of closed Qualifier kinds
  ios_version_untested     INTEGER NOT NULL CHECK (ios_version_untested IN (0, 1)),
  include_recently_deleted INTEGER NOT NULL CHECK (include_recently_deleted IN (0, 1)),
  copy_attachments         INTEGER NOT NULL CHECK (copy_attachments IN (0, 1)),
  capabilities             TEXT    NOT NULL,         -- JSON: closed booleans for optional tables/columns
  counts                   TEXT,                     -- JSON: ImportCounts (content-free), written at every batch commit
  full_observation         INTEGER CHECK (full_observation IN (0, 1)), -- set at a terminal state (spec 8.7)
  resumes                  INTEGER NOT NULL DEFAULT 0,
  CHECK ((state = 0) = (ended_at_ns IS NULL)),
  CHECK ((order_class = 1) = (order_key IS NOT NULL)),
  CHECK ((order_key IS NULL) = (order_key_source IS NULL))
) STRICT;
CREATE INDEX import_session_state ON import_session(state);

-- Written in the same transaction as the batch it describes.
CREATE TABLE import_cursor (
  session_id   INTEGER NOT NULL REFERENCES import_session(id),
  pass         INTEGER NOT NULL,                  -- closed Pass code
  last_rowid   INTEGER,                           -- NULL = Cursor::Start
  rows_done    INTEGER NOT NULL,                  -- records completed (written, skipped or counted)
  done         INTEGER NOT NULL CHECK (done IN (0, 1)),
  PRIMARY KEY (session_id, pass)
) STRICT, WITHOUT ROWID;

CREATE TABLE import_issue (
  session_id INTEGER NOT NULL REFERENCES import_session(id),
  pass       INTEGER NOT NULL,
  kind       INTEGER NOT NULL,                    -- closed IssueCode (stop reasons included)
  count      INTEGER NOT NULL,
  PRIMARY KEY (session_id, pass, kind)
) STRICT, WITHOUT ROWID;

-- Keys that occur more than once in this session's source (128-bit truncated SHA-256 of the
-- tagged key encoding). Every occurrence of such a key is imported unkeyed (spec 6.2).
CREATE TABLE import_dup_key (
  session_id INTEGER NOT NULL REFERENCES import_session(id),
  entity     INTEGER NOT NULL CHECK (entity BETWEEN 1 AND 4),  -- 1 handle 2 chat 3 message 4 attachment
  key_digest BLOB    NOT NULL CHECK (length(key_digest) = 16),
  PRIMARY KEY (session_id, entity, key_digest)
) STRICT, WITHOUT ROWID;

-- Occurrence counters for unkeyed fingerprints, committed with each batch; emptied when the session ends.
CREATE TABLE import_occurrence (
  session_id INTEGER NOT NULL REFERENCES import_session(id),
  entity     INTEGER NOT NULL CHECK (entity BETWEEN 1 AND 4),
  fp         BLOB    NOT NULL CHECK (length(fp) = 32),
  seen       INTEGER NOT NULL CHECK (seen >= 1),
  PRIMARY KEY (session_id, entity, fp)
) STRICT, WITHOUT ROWID;

-- Source ROWID -> archive id, for the running session only; emptied when it ends.
CREATE TABLE import_rowid_map (
  session_id  INTEGER NOT NULL REFERENCES import_session(id),
  src         INTEGER NOT NULL CHECK (src BETWEEN 1 AND 4),  -- 1 handle, 2 chat, 3 message, 4 attachment
  src_rowid   INTEGER NOT NULL,
  archive_id  INTEGER NOT NULL,
  PRIMARY KEY (session_id, src, src_rowid)
) STRICT, WITHOUT ROWID;

-- Files about to be placed in attachments/, committed before the rename (spec 9.1).
CREATE TABLE blob_intent (
  sha256      BLOB    NOT NULL CHECK (length(sha256) = 32),
  session_id  INTEGER NOT NULL REFERENCES import_session(id),
  PRIMARY KEY (sha256, session_id)
) STRICT, WITHOUT ROWID;

-- Rare per-value detail about the value currently stored.
CREATE TABLE value_issue (
  entity   INTEGER NOT NULL,                      -- closed Entity code
  row_id   INTEGER NOT NULL,
  col      INTEGER NOT NULL,                      -- closed ArchiveColumn code, never reused
  kind     INTEGER NOT NULL,                      -- closed ValueIssueKind: 1 oversized, 2 raw kept,
                                                  -- 3 text stored as BLOB in the source, 16+ IssueKind
  bytes    INTEGER,                               -- oversized: octet_length from SQLite
  raw      ANY,                                   -- the raw value under the column cap, when kept
  PRIMARY KEY (entity, row_id, col, kind)
) STRICT, WITHOUT ROWID;

-- Every distinct observed value of a history column other than the current one (spec 8.7).
CREATE TABLE value_history (
  entity            INTEGER NOT NULL,
  row_id            INTEGER NOT NULL,
  col               INTEGER NOT NULL,
  value_hash        BLOB    NOT NULL CHECK (length(value_hash) = 32),  -- over the tagged encoding
  value             ANY,                                              -- NULL: observed as SQL NULL
  first_rank        BLOB    NOT NULL CHECK (length(first_rank) = 41),
  first_session_id  INTEGER NOT NULL REFERENCES import_session(id),
  PRIMARY KEY (entity, row_id, col, value_hash)
) STRICT, WITHOUT ROWID;

CREATE TABLE handle (
  id                   INTEGER PRIMARY KEY,
  key_kind             INTEGER NOT NULL CHECK (key_kind IN (1, 2)), -- 1 exact (id, service); 2 unkeyed
  natural_key          BLOB,
  unkeyed_fp           BLOB,    unkeyed_occ INTEGER,
  address              BLOB,    address_st INTEGER,          -- handle.id
  service              BLOB,    service_st INTEGER,
  service_class        INTEGER,                              -- derived, closed ServiceClass code
  country              BLOB,    country_st INTEGER,
  uncanonicalized_id   BLOB,    uncanonicalized_id_st INTEGER,
  person_centric_id    BLOB,    person_centric_id_st INTEGER,
  first_session_id     INTEGER NOT NULL REFERENCES import_session(id),
  first_rank           BLOB    NOT NULL CHECK (length(first_rank) = 41),
  last_session_id      INTEGER NOT NULL REFERENCES import_session(id),
  last_rank            BLOB    NOT NULL CHECK (length(last_rank) = 41),
  CHECK (key_kind <> 1 OR (natural_key IS NOT NULL AND unkeyed_fp IS NULL AND unkeyed_occ IS NULL)),
  CHECK (key_kind <> 2 OR (natural_key IS NULL AND length(unkeyed_fp) = 32 AND unkeyed_occ >= 0)),
  CHECK ((address IS NULL) <> (address_st IS NULL)),
  CHECK ((service IS NULL) <> (service_st IS NULL)),
  CHECK ((country IS NULL) <> (country_st IS NULL)),
  CHECK ((uncanonicalized_id IS NULL) <> (uncanonicalized_id_st IS NULL)),
  CHECK ((person_centric_id IS NULL) <> (person_centric_id_st IS NULL))
) STRICT;
CREATE UNIQUE INDEX handle_natural ON handle(natural_key)             WHERE key_kind = 1;
CREATE UNIQUE INDEX handle_unkeyed ON handle(unkeyed_fp, unkeyed_occ) WHERE key_kind = 2;

CREATE TABLE chat (
  id                   INTEGER PRIMARY KEY,
  key_kind             INTEGER NOT NULL CHECK (key_kind IN (1, 2)),
  guid                 BLOB,    guid_st INTEGER,
  unkeyed_fp           BLOB,    unkeyed_occ INTEGER,
  style                INTEGER, style_st INTEGER,            -- raw; meaning UNVERIFIED (D39)
  chat_identifier      BLOB,    chat_identifier_st INTEGER,
  service_name         BLOB,    service_name_st INTEGER,
  service_class        INTEGER,
  display_name         BLOB,    display_name_st INTEGER,
  room_name            BLOB,    room_name_st INTEGER,
  group_id             BLOB,    group_id_st INTEGER,
  properties_bytes     INTEGER, properties_bytes_st INTEGER, -- size; raw bytes (R5, deferred) go to value_issue kind 2
  is_archived          INTEGER, is_archived_st INTEGER,
  is_filtered          INTEGER, is_filtered_st INTEGER,      -- raw
  is_blackholed        INTEGER, is_blackholed_st INTEGER,
  is_pending_review    INTEGER, is_pending_review_st INTEGER,
  is_recovered         INTEGER, is_recovered_st INTEGER,
  last_read_raw INTEGER, last_read_st INTEGER, last_read_unit INTEGER, last_read_utc_ns INTEGER,
  first_session_id     INTEGER NOT NULL REFERENCES import_session(id),
  first_rank           BLOB    NOT NULL CHECK (length(first_rank) = 41),
  last_session_id      INTEGER NOT NULL REFERENCES import_session(id),
  last_rank            BLOB    NOT NULL CHECK (length(last_rank) = 41),
  CHECK (key_kind <> 1 OR (guid IS NOT NULL AND unkeyed_fp IS NULL AND unkeyed_occ IS NULL)),
  CHECK (key_kind <> 2 OR (length(unkeyed_fp) = 32 AND unkeyed_occ >= 0)),
  CHECK ((guid IS NULL) <> (guid_st IS NULL)),
  CHECK ((style IS NULL) <> (style_st IS NULL)),
  CHECK ((chat_identifier IS NULL) <> (chat_identifier_st IS NULL)),
  CHECK ((service_name IS NULL) <> (service_name_st IS NULL)),
  CHECK ((display_name IS NULL) <> (display_name_st IS NULL)),
  CHECK ((room_name IS NULL) <> (room_name_st IS NULL)),
  CHECK ((group_id IS NULL) <> (group_id_st IS NULL)),
  CHECK ((properties_bytes IS NULL) <> (properties_bytes_st IS NULL)),
  CHECK ((is_archived IS NULL) <> (is_archived_st IS NULL)),
  CHECK ((is_filtered IS NULL) <> (is_filtered_st IS NULL)),
  CHECK ((is_blackholed IS NULL) <> (is_blackholed_st IS NULL)),
  CHECK ((is_pending_review IS NULL) <> (is_pending_review_st IS NULL)),
  CHECK ((is_recovered IS NULL) <> (is_recovered_st IS NULL)),
  CHECK ((last_read_raw IS NULL) <> (last_read_st IS NULL)),
  CHECK ((last_read_raw IS NULL) = (last_read_unit IS NULL)),
  CHECK ((last_read_utc_ns IS NOT NULL) = (last_read_unit IS NOT NULL AND last_read_unit IN (2, 3)))
) STRICT;
CREATE UNIQUE INDEX chat_guid    ON chat(guid)                    WHERE key_kind = 1;
CREATE UNIQUE INDEX chat_unkeyed ON chat(unkeyed_fp, unkeyed_occ) WHERE key_kind = 2;

CREATE TABLE message (
  id                   INTEGER PRIMARY KEY,
  key_kind             INTEGER NOT NULL CHECK (key_kind IN (1, 2)),
  guid                 BLOB,    guid_st INTEGER,
  unkeyed_fp           BLOB,    unkeyed_occ INTEGER,
  is_from_me           INTEGER, is_from_me_st INTEGER,       -- raw
  handle_rowid         INTEGER, handle_rowid_st INTEGER,     -- raw source value (session-local meaning)
  handle_ref           INTEGER REFERENCES handle(id),        -- resolved; NULL when 0, missing or unresolved
  other_handle_rowid   INTEGER, other_handle_rowid_st INTEGER,
  other_handle_ref     INTEGER REFERENCES handle(id),
  destination_caller_id BLOB,   destination_caller_id_st INTEGER, -- meaning UNVERIFIED; may be the owner's own address
  service              BLOB,    service_st INTEGER,
  service_class        INTEGER,
  date_raw INTEGER, date_st INTEGER, date_unit INTEGER, date_utc_ns INTEGER,
  date_read_raw INTEGER, date_read_st INTEGER, date_read_unit INTEGER, date_read_utc_ns INTEGER,
  date_delivered_raw INTEGER, date_delivered_st INTEGER, date_delivered_unit INTEGER, date_delivered_utc_ns INTEGER,
  date_played_raw INTEGER, date_played_st INTEGER, date_played_unit INTEGER, date_played_utc_ns INTEGER,
  date_edited_raw INTEGER, date_edited_st INTEGER, date_edited_unit INTEGER, date_edited_utc_ns INTEGER,
  date_retracted_raw INTEGER, date_retracted_st INTEGER, date_retracted_unit INTEGER, date_retracted_utc_ns INTEGER,
  is_read              INTEGER, is_read_st INTEGER,          -- stored, never shown as a state (I5)
  is_delivered         INTEGER, is_delivered_st INTEGER,
  is_sent              INTEGER, is_sent_st INTEGER,
  is_audio_message     INTEGER, is_audio_message_st INTEGER,
  cache_has_attachments INTEGER, cache_has_attachments_st INTEGER, -- a cache flag, not a fact
  subject              BLOB,    subject_st INTEGER,
  item_type            INTEGER, item_type_st INTEGER,
  group_action_type    INTEGER, group_action_type_st INTEGER,
  item_kind            INTEGER NOT NULL,                     -- derived ItemKind code (derived_by)
  group_title          BLOB,    group_title_st INTEGER,
  share_status         INTEGER, share_status_st INTEGER,
  share_direction      INTEGER, share_direction_st INTEGER,
  assoc_type           INTEGER, assoc_type_st INTEGER,       -- associated_message_type, raw, always recorded
  assoc_guid_raw       BLOB,    assoc_guid_raw_st INTEGER,   -- associated_message_guid, raw ('' kept), always recorded
  assoc_present        INTEGER NOT NULL CHECK (assoc_present IN (0, 1, 2)), -- 0 no, 1 yes, 2 not determinable
  assoc_kind           INTEGER,                              -- derived AssociatedKind code
  assoc_target_shape   INTEGER,                              -- 1 part, 2 bubble, 3 bare, 4 unparsed
  assoc_target_guid    BLOB,                                 -- parsed target GUID (exact bytes)
  assoc_target_part    INTEGER,
  assoc_emoji          BLOB,    assoc_emoji_st INTEGER,
  assoc_range_location INTEGER, assoc_range_location_st INTEGER,
  assoc_range_length   INTEGER, assoc_range_length_st INTEGER,
  thread_originator_guid BLOB,  thread_originator_guid_st INTEGER,
  thread_originator_part BLOB,  thread_originator_part_st INTEGER, -- raw; format UNVERIFIED
  reply_to_guid        BLOB,    reply_to_guid_st INTEGER,     -- not a reply marker
  balloon_bundle_id    BLOB,    balloon_bundle_id_st INTEGER,
  expressive_send_style_id BLOB, expressive_send_style_id_st INTEGER,
  payload_data         BLOB,    payload_data_st INTEGER,      -- raw, under a 1 MiB cap (R3)
  part_count           INTEGER, part_count_st INTEGER,
  schedule_type        INTEGER, schedule_type_st INTEGER,     -- non-zero: date is the planned send time
  schedule_state       INTEGER, schedule_state_st INTEGER,
  filter_action        INTEGER, filter_action_st INTEGER,
  filter_sub_action    INTEGER, filter_sub_action_st INTEGER,
  error                INTEGER, error_st INTEGER,
  was_downgraded       INTEGER, was_downgraded_st INTEGER,
  current_body_id      INTEGER REFERENCES message_body(id) DEFERRABLE INITIALLY DEFERRED,
  sort_key             BLOB    NOT NULL,                      -- total order key (spec 6.5)
  first_src_rowid      INTEGER NOT NULL,                      -- source ROWID in the session that set first_rank
  derived_by           INTEGER NOT NULL,                      -- deriver_version that filled derived columns
  first_session_id     INTEGER NOT NULL REFERENCES import_session(id),
  first_rank           BLOB    NOT NULL CHECK (length(first_rank) = 41),
  last_session_id      INTEGER NOT NULL REFERENCES import_session(id),
  last_rank            BLOB    NOT NULL CHECK (length(last_rank) = 41),
  CHECK (key_kind <> 1 OR (guid IS NOT NULL AND unkeyed_fp IS NULL AND unkeyed_occ IS NULL)),
  CHECK (key_kind <> 2 OR (length(unkeyed_fp) = 32 AND unkeyed_occ >= 0)),
  CHECK ((guid IS NULL) <> (guid_st IS NULL)),
  CHECK ((is_from_me IS NULL) <> (is_from_me_st IS NULL)),
  CHECK ((handle_rowid IS NULL) <> (handle_rowid_st IS NULL)),
  CHECK ((other_handle_rowid IS NULL) <> (other_handle_rowid_st IS NULL)),
  CHECK ((destination_caller_id IS NULL) <> (destination_caller_id_st IS NULL)),
  CHECK ((service IS NULL) <> (service_st IS NULL)),
  CHECK ((date_raw IS NULL) <> (date_st IS NULL)),
  CHECK ((date_raw IS NULL) = (date_unit IS NULL)),
  CHECK ((date_utc_ns IS NOT NULL) = (date_unit IS NOT NULL AND date_unit IN (2, 3))),
  CHECK ((date_read_raw IS NULL) <> (date_read_st IS NULL)),
  CHECK ((date_read_raw IS NULL) = (date_read_unit IS NULL)),
  CHECK ((date_read_utc_ns IS NOT NULL) = (date_read_unit IS NOT NULL AND date_read_unit IN (2, 3))),
  CHECK ((date_delivered_raw IS NULL) <> (date_delivered_st IS NULL)),
  CHECK ((date_delivered_raw IS NULL) = (date_delivered_unit IS NULL)),
  CHECK ((date_delivered_utc_ns IS NOT NULL) = (date_delivered_unit IS NOT NULL AND date_delivered_unit IN (2, 3))),
  CHECK ((date_played_raw IS NULL) <> (date_played_st IS NULL)),
  CHECK ((date_played_raw IS NULL) = (date_played_unit IS NULL)),
  CHECK ((date_played_utc_ns IS NOT NULL) = (date_played_unit IS NOT NULL AND date_played_unit IN (2, 3))),
  CHECK ((date_edited_raw IS NULL) <> (date_edited_st IS NULL)),
  CHECK ((date_edited_raw IS NULL) = (date_edited_unit IS NULL)),
  CHECK ((date_edited_utc_ns IS NOT NULL) = (date_edited_unit IS NOT NULL AND date_edited_unit IN (2, 3))),
  CHECK ((date_retracted_raw IS NULL) <> (date_retracted_st IS NULL)),
  CHECK ((date_retracted_raw IS NULL) = (date_retracted_unit IS NULL)),
  CHECK ((date_retracted_utc_ns IS NOT NULL) = (date_retracted_unit IS NOT NULL AND date_retracted_unit IN (2, 3))),
  CHECK ((is_read IS NULL) <> (is_read_st IS NULL)),
  CHECK ((is_delivered IS NULL) <> (is_delivered_st IS NULL)),
  CHECK ((is_sent IS NULL) <> (is_sent_st IS NULL)),
  CHECK ((is_audio_message IS NULL) <> (is_audio_message_st IS NULL)),
  CHECK ((cache_has_attachments IS NULL) <> (cache_has_attachments_st IS NULL)),
  CHECK ((subject IS NULL) <> (subject_st IS NULL)),
  CHECK ((item_type IS NULL) <> (item_type_st IS NULL)),
  CHECK ((group_action_type IS NULL) <> (group_action_type_st IS NULL)),
  CHECK ((group_title IS NULL) <> (group_title_st IS NULL)),
  CHECK ((share_status IS NULL) <> (share_status_st IS NULL)),
  CHECK ((share_direction IS NULL) <> (share_direction_st IS NULL)),
  CHECK ((assoc_type IS NULL) <> (assoc_type_st IS NULL)),
  CHECK ((assoc_guid_raw IS NULL) <> (assoc_guid_raw_st IS NULL)),
  CHECK ((assoc_emoji IS NULL) <> (assoc_emoji_st IS NULL)),
  CHECK ((assoc_range_location IS NULL) <> (assoc_range_location_st IS NULL)),
  CHECK ((assoc_range_length IS NULL) <> (assoc_range_length_st IS NULL)),
  CHECK (assoc_present = 1 OR (assoc_kind IS NULL AND assoc_target_shape IS NULL
         AND assoc_target_guid IS NULL AND assoc_target_part IS NULL)),
  CHECK (assoc_present <> 1 OR assoc_target_shape IS NOT NULL OR assoc_kind IS NOT NULL),
  CHECK ((thread_originator_guid IS NULL) <> (thread_originator_guid_st IS NULL)),
  CHECK ((thread_originator_part IS NULL) <> (thread_originator_part_st IS NULL)),
  CHECK ((reply_to_guid IS NULL) <> (reply_to_guid_st IS NULL)),
  CHECK ((balloon_bundle_id IS NULL) <> (balloon_bundle_id_st IS NULL)),
  CHECK ((expressive_send_style_id IS NULL) <> (expressive_send_style_id_st IS NULL)),
  CHECK ((payload_data IS NULL) <> (payload_data_st IS NULL)),
  CHECK ((part_count IS NULL) <> (part_count_st IS NULL)),
  CHECK ((schedule_type IS NULL) <> (schedule_type_st IS NULL)),
  CHECK ((schedule_state IS NULL) <> (schedule_state_st IS NULL)),
  CHECK ((filter_action IS NULL) <> (filter_action_st IS NULL)),
  CHECK ((filter_sub_action IS NULL) <> (filter_sub_action_st IS NULL)),
  CHECK ((error IS NULL) <> (error_st IS NULL)),
  CHECK ((was_downgraded IS NULL) <> (was_downgraded_st IS NULL))
) STRICT;
CREATE UNIQUE INDEX message_guid    ON message(guid)                    WHERE key_kind = 1;
CREATE UNIQUE INDEX message_unkeyed ON message(unkeyed_fp, unkeyed_occ) WHERE key_kind = 2;
CREATE INDEX message_handle      ON message(handle_ref)             WHERE handle_ref IS NOT NULL;
CREATE INDEX message_assoc       ON message(assoc_target_guid)      WHERE assoc_target_guid IS NOT NULL;
CREATE INDEX message_reply       ON message(thread_originator_guid) WHERE thread_originator_guid IS NOT NULL;
CREATE INDEX message_sort        ON message(sort_key, id);

-- One row per distinct raw body state observed (raw_hash). Raw inputs are kept so a
-- better decoder can re-derive in place (decoder_version) without a new version.
CREATE TABLE message_body (
  id                   INTEGER PRIMARY KEY,
  message_id           INTEGER NOT NULL REFERENCES message(id),
  raw_hash             BLOB    NOT NULL CHECK (length(raw_hash) = 32),
  text_raw             BLOB,    text_raw_st INTEGER,          -- message.text, exact bytes
  attributed_raw       BLOB,    attributed_raw_st INTEGER,    -- message.attributedBody (cap 16 MiB)
  summary_raw          BLOB,    summary_raw_st INTEGER,       -- message_summary_info (cap 1 MiB); holds ec[*].t edit text
  attributed_outcome   INTEGER NOT NULL CHECK (attributed_outcome BETWEEN 0 AND 6),
                       -- 0 absent 1 null 2 oversized 3 unreadable 4 not decoded 5 decoded 6 decode failed
  attributed_error     INTEGER,                              -- TsErrorKind code when 6
  run_issue            INTEGER,                              -- RunIssue code
  unknown_attr_keys    INTEGER,                              -- NULL unless decoded
  unknown_attr_classes INTEGER,
  display_text         TEXT,                                 -- derived; the FTS content column
  display_source       INTEGER CHECK (display_source IN (1, 2, 3)),
                       -- 1 decoded attributedBody, 2 text column (valid UTF-8), 3 text column, lossy
  display_none_reason  INTEGER,                              -- closed code when display_text IS NULL
  sources_differ       INTEGER CHECK (sources_differ IN (0, 1)), -- NULL: not comparable
  summary_outcome      INTEGER NOT NULL CHECK (summary_outcome BETWEEN 0 AND 4),
                       -- 0 absent 1 null 2 oversized 3 unreadable 4 parsed
  otr                  INTEGER,                              -- parsed `otr` entry count, when a value
  ec_state             INTEGER,                              -- FieldOutcome 0 missing 1 value 2 bad; NULL: not evaluated
  rp_state             INTEGER,
  has_tmp              INTEGER CHECK (has_tmp IN (0, 1)),
  summary_unknown_keys INTEGER,
  edit_label           INTEGER NOT NULL,                     -- EditLabel code; 0 = undeterminable
  edit_label_a         INTEGER,
  edit_label_b         INTEGER,
  decoder_version      INTEGER NOT NULL,
  first_session_id     INTEGER NOT NULL REFERENCES import_session(id),
  first_rank           BLOB    NOT NULL CHECK (length(first_rank) = 41),
  last_session_id      INTEGER NOT NULL REFERENCES import_session(id),
  last_rank            BLOB    NOT NULL CHECK (length(last_rank) = 41),
  UNIQUE (message_id, raw_hash),
  CHECK ((text_raw IS NULL) <> (text_raw_st IS NULL)),
  CHECK ((attributed_raw IS NULL) <> (attributed_raw_st IS NULL)),
  CHECK ((summary_raw IS NULL) <> (summary_raw_st IS NULL)),
  CHECK ((attributed_outcome = 5) = (unknown_attr_keys IS NOT NULL)),
  CHECK ((unknown_attr_keys IS NULL) = (unknown_attr_classes IS NULL)),
  CHECK ((attributed_outcome = 6) = (attributed_error IS NOT NULL)),
  CHECK ((display_text IS NULL) = (display_source IS NULL)),
  CHECK ((display_text IS NULL) = (display_none_reason IS NOT NULL)),
  CHECK ((summary_outcome = 4) = (ec_state IS NOT NULL)),
  CHECK ((ec_state IS NULL) = (rp_state IS NULL)),
  CHECK ((ec_state IS NULL) = (has_tmp IS NULL)),
  CHECK ((ec_state IS NULL) = (summary_unknown_keys IS NULL))
) STRICT;

CREATE TABLE message_body_part (
  body_id          INTEGER NOT NULL REFERENCES message_body(id),
  ordinal          INTEGER NOT NULL,
  part_index       INTEGER,
  utf16_start      INTEGER NOT NULL, utf16_end INTEGER NOT NULL,
  utf8_start       INTEGER NOT NULL, utf8_end  INTEGER NOT NULL,
  attachment_guid  BLOB,                                     -- equality with attachment.guid UNVERIFIED
  mention          BLOB,
  flags            INTEGER NOT NULL,                         -- AttrFlags bits
  keys_present     INTEGER NOT NULL,                         -- AttrKeysPresent bits
  text_effect      INTEGER,
  PRIMARY KEY (body_id, ordinal)
) STRICT, WITHOUT ROWID;

CREATE TABLE message_edit_entry (
  body_id       INTEGER NOT NULL REFERENCES message_body(id),
  part          INTEGER NOT NULL,
  ordinal       INTEGER NOT NULL,                            -- stored order; "oldest first" is not claimed
  at_kind       INTEGER,                                     -- SummaryTime kind; NULL when absent
  at_raw        ANY,                                         -- integer, real or plist date as read
  at_utc_ns     INTEGER,
  body_present  INTEGER NOT NULL CHECK (body_present IN (0, 1)),
  PRIMARY KEY (body_id, part, ordinal)
) STRICT, WITHOUT ROWID;

CREATE TABLE message_unsent_part (
  body_id  INTEGER NOT NULL REFERENCES message_body(id),
  ordinal  INTEGER NOT NULL,
  part     INTEGER NOT NULL,
  PRIMARY KEY (body_id, ordinal)
) STRICT, WITHOUT ROWID;

-- Full-text index over every body version (current and earlier). External content:
-- snippet() and highlight() work, 'rebuild' is possible, and 'integrity-check' with
-- rank 1 compares the index against message_body.
CREATE VIRTUAL TABLE message_fts USING fts5(
  display_text,
  content = 'message_body',
  content_rowid = 'id',
  tokenize = 'unicode61 remove_diacritics 2',
  detail = full
);
CREATE TRIGGER message_body_ai AFTER INSERT ON message_body BEGIN
  INSERT INTO message_fts(rowid, display_text) VALUES (new.id, new.display_text);
END;
CREATE TRIGGER message_body_ad AFTER DELETE ON message_body BEGIN
  INSERT INTO message_fts(message_fts, rowid, display_text) VALUES ('delete', old.id, old.display_text);
END;
CREATE TRIGGER message_body_au AFTER UPDATE OF display_text ON message_body
WHEN old.display_text IS NOT new.display_text BEGIN
  INSERT INTO message_fts(message_fts, rowid, display_text) VALUES ('delete', old.id, old.display_text);
  INSERT INTO message_fts(rowid, display_text) VALUES (new.id, new.display_text);
END;

CREATE TABLE attachment (
  id                   INTEGER PRIMARY KEY,
  key_kind             INTEGER NOT NULL CHECK (key_kind IN (1, 2)),
  guid                 BLOB,    guid_st INTEGER,
  unkeyed_fp           BLOB,    unkeyed_occ INTEGER,
  original_guid        BLOB,    original_guid_st INTEGER,
  filename             BLOB,    filename_st INTEGER,        -- untrusted lookup key, never a path
  uti                  BLOB,    uti_st INTEGER,
  mime_type            BLOB,    mime_type_st INTEGER,
  transfer_name        BLOB,    transfer_name_st INTEGER,   -- sanitized only at export (M6)
  total_bytes          INTEGER, total_bytes_st INTEGER,     -- declared by sms.db; not trusted for space
  is_sticker           INTEGER, is_sticker_st INTEGER,
  hide_attachment      INTEGER, hide_attachment_st INTEGER,
  is_outgoing          INTEGER, is_outgoing_st INTEGER,
  transfer_state       INTEGER, transfer_state_st INTEGER,  -- raw
  created_raw INTEGER, created_st INTEGER, created_unit INTEGER, created_utc_ns INTEGER,
  start_raw   INTEGER, start_st   INTEGER, start_unit   INTEGER, start_utc_ns   INTEGER,
  emoji_description    BLOB,    emoji_description_st INTEGER, -- Genmoji prompt (content)
  current_blob_id      INTEGER REFERENCES blob(id),          -- follows the order rule
  first_session_id     INTEGER NOT NULL REFERENCES import_session(id),
  first_rank           BLOB    NOT NULL CHECK (length(first_rank) = 41),
  last_session_id      INTEGER NOT NULL REFERENCES import_session(id),
  last_rank            BLOB    NOT NULL CHECK (length(last_rank) = 41),
  CHECK (key_kind <> 1 OR (guid IS NOT NULL AND unkeyed_fp IS NULL AND unkeyed_occ IS NULL)),
  CHECK (key_kind <> 2 OR (length(unkeyed_fp) = 32 AND unkeyed_occ >= 0)),
  CHECK ((guid IS NULL) <> (guid_st IS NULL)),
  CHECK ((original_guid IS NULL) <> (original_guid_st IS NULL)),
  CHECK ((filename IS NULL) <> (filename_st IS NULL)),
  CHECK ((uti IS NULL) <> (uti_st IS NULL)),
  CHECK ((mime_type IS NULL) <> (mime_type_st IS NULL)),
  CHECK ((transfer_name IS NULL) <> (transfer_name_st IS NULL)),
  CHECK ((total_bytes IS NULL) <> (total_bytes_st IS NULL)),
  CHECK ((is_sticker IS NULL) <> (is_sticker_st IS NULL)),
  CHECK ((hide_attachment IS NULL) <> (hide_attachment_st IS NULL)),
  CHECK ((is_outgoing IS NULL) <> (is_outgoing_st IS NULL)),
  CHECK ((transfer_state IS NULL) <> (transfer_state_st IS NULL)),
  CHECK ((created_raw IS NULL) <> (created_st IS NULL)),
  CHECK ((created_raw IS NULL) = (created_unit IS NULL)),
  CHECK ((created_utc_ns IS NOT NULL) = (created_unit IS NOT NULL AND created_unit IN (2, 3))),
  CHECK ((start_raw IS NULL) <> (start_st IS NULL)),
  CHECK ((start_raw IS NULL) = (start_unit IS NULL)),
  CHECK ((start_utc_ns IS NOT NULL) = (start_unit IS NOT NULL AND start_unit IN (2, 3))),
  CHECK ((emoji_description IS NULL) <> (emoji_description_st IS NULL))
) STRICT;
CREATE UNIQUE INDEX attachment_guid    ON attachment(guid)                    WHERE key_kind = 1;
CREATE UNIQUE INDEX attachment_unkeyed ON attachment(unkeyed_fp, unkeyed_occ) WHERE key_kind = 2;
CREATE INDEX attachment_blob_ref ON attachment(current_blob_id) WHERE current_blob_id IS NOT NULL;

-- Content-addressed file store. storage_name is separate from sha256 so M8 can rename
-- files (keyed names) by migration without touching referencing rows.
CREATE TABLE blob (
  id                INTEGER PRIMARY KEY,
  sha256            BLOB    NOT NULL UNIQUE CHECK (length(sha256) = 32),
  byte_len          INTEGER NOT NULL CHECK (byte_len >= 0),
  storage_name      TEXT    NOT NULL UNIQUE CHECK (length(storage_name) = 64
                      AND storage_name NOT GLOB '*[^0-9a-f]*'
                      AND storage_name = lower(hex(sha256))),   -- name_scheme 1; the file is attachments/<first 2>/<storage_name>
  first_session_id  INTEGER NOT NULL REFERENCES import_session(id),
  verified_at_ns    INTEGER,                                  -- last full re-hash by the verifier
  verify_state      INTEGER                                   -- NULL never verified, 0 ok, else VerifyProblem code
) STRICT;

-- What each session observed for each attachment (lookup, copy, bytes).
CREATE TABLE attachment_observation (
  attachment_id         INTEGER NOT NULL REFERENCES attachment(id),
  session_id            INTEGER NOT NULL REFERENCES import_session(id),
  path_class            INTEGER NOT NULL,                     -- PathClass code
  lookup                INTEGER NOT NULL,                     -- LookupOutcome / BlobProbe code
  copy                  INTEGER NOT NULL,                     -- CopyOutcome code
  blob_id               INTEGER REFERENCES blob(id),          -- bytes read in this session, if any
  size_matched_manifest INTEGER CHECK (size_matched_manifest IN (0, 1)), -- NULL: no Size recorded, or not read
  PRIMARY KEY (attachment_id, session_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX attachment_observation_by_blob ON attachment_observation(blob_id) WHERE blob_id IS NOT NULL;

CREATE TABLE chat_handle (
  chat_id           INTEGER NOT NULL REFERENCES chat(id),
  handle_id         INTEGER NOT NULL REFERENCES handle(id),
  first_session_id  INTEGER NOT NULL REFERENCES import_session(id),
  last_session_id   INTEGER NOT NULL REFERENCES import_session(id),
  last_rank         BLOB    NOT NULL CHECK (length(last_rank) = 41),
  PRIMARY KEY (chat_id, handle_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX chat_handle_by_handle ON chat_handle(handle_id);

CREATE TABLE chat_message (
  chat_id           INTEGER NOT NULL REFERENCES chat(id),
  message_id        INTEGER NOT NULL REFERENCES message(id),
  sort_key          BLOB    NOT NULL,                         -- copy of message.sort_key (trigger)
  join_date_raw INTEGER, join_date_st INTEGER, join_date_unit INTEGER, join_date_utc_ns INTEGER, -- unit UNVERIFIED
  first_session_id  INTEGER NOT NULL REFERENCES import_session(id),
  last_session_id   INTEGER NOT NULL REFERENCES import_session(id),
  last_rank         BLOB    NOT NULL CHECK (length(last_rank) = 41),
  PRIMARY KEY (chat_id, message_id),
  CHECK ((join_date_raw IS NULL) <> (join_date_st IS NULL)),
  CHECK ((join_date_raw IS NULL) = (join_date_unit IS NULL)),
  CHECK ((join_date_utc_ns IS NOT NULL) = (join_date_unit IS NOT NULL AND join_date_unit IN (2, 3)))
) STRICT, WITHOUT ROWID;
CREATE INDEX chat_message_timeline   ON chat_message(chat_id, sort_key, message_id);
CREATE INDEX chat_message_by_message ON chat_message(message_id);
CREATE TRIGGER message_sort_au AFTER UPDATE OF sort_key ON message
WHEN old.sort_key IS NOT new.sort_key BEGIN
  UPDATE chat_message SET sort_key = new.sort_key WHERE message_id = new.id;
END;

CREATE TABLE message_attachment (
  message_id        INTEGER NOT NULL REFERENCES message(id),
  attachment_id     INTEGER NOT NULL REFERENCES attachment(id),
  ord_rank          BLOB    NOT NULL CHECK (length(ord_rank) = 41), -- rank of the session that set ord_rowid
  ord_rowid         INTEGER NOT NULL,                         -- join ROWID in that session (spec 6.5)
  first_session_id  INTEGER NOT NULL REFERENCES import_session(id),
  last_session_id   INTEGER NOT NULL REFERENCES import_session(id),
  last_rank         BLOB    NOT NULL CHECK (length(last_rank) = 41),
  PRIMARY KEY (message_id, attachment_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX message_attachment_by_attachment ON message_attachment(attachment_id);

-- One row per chat_recoverable_message_join row observed, per session.
CREATE TABLE message_recently_deleted (
  session_id        INTEGER NOT NULL REFERENCES import_session(id),
  src_rowid         INTEGER NOT NULL,                         -- join ROWID (session-local)
  message_id        INTEGER NOT NULL REFERENCES message(id),
  chat_ref          INTEGER REFERENCES chat(id),
  chat_rowid        INTEGER, chat_rowid_st INTEGER,           -- raw source value
  delete_raw INTEGER, delete_st INTEGER, delete_unit INTEGER, delete_utc_ns INTEGER, -- unit UNVERIFIED
  ck_sync_state     INTEGER, ck_sync_state_st INTEGER,
  PRIMARY KEY (session_id, src_rowid),
  CHECK ((chat_rowid IS NULL) <> (chat_rowid_st IS NULL)),
  CHECK ((delete_raw IS NULL) <> (delete_st IS NULL)),
  CHECK ((delete_raw IS NULL) = (delete_unit IS NULL)),
  CHECK ((delete_utc_ns IS NOT NULL) = (delete_unit IS NOT NULL AND delete_unit IN (2, 3))),
  CHECK ((ck_sync_state IS NULL) <> (ck_sync_state_st IS NULL))
) STRICT, WITHOUT ROWID;
CREATE INDEX message_recently_deleted_by_message ON message_recently_deleted(message_id);

-- Which lineages observed a row, for "not in the latest import" (spec 8.7).
CREATE TABLE lineage_seen (
  entity            INTEGER NOT NULL CHECK (entity IN (2, 3, 4)), -- 2 chat 3 message 4 attachment
  row_id            INTEGER NOT NULL,
  lineage_id        INTEGER NOT NULL REFERENCES lineage(id),
  last_rank         BLOB    NOT NULL CHECK (length(last_rank) = 41),
  last_session_id   INTEGER NOT NULL REFERENCES import_session(id),
  PRIMARY KEY (entity, row_id, lineage_id)
) STRICT, WITHOUT ROWID;

-- Derived; recomputed at the end of every import. Deletable and rebuildable.
CREATE TABLE chat_stats (
  chat_id         INTEGER PRIMARY KEY REFERENCES chat(id),
  message_count   INTEGER NOT NULL,
  undated_count   INTEGER NOT NULL,
  first_sort_ns   INTEGER,
  last_sort_ns    INTEGER
) STRICT;

CREATE TABLE archive_stats (
  id              INTEGER PRIMARY KEY CHECK (id = 1),
  handles INTEGER NOT NULL, chats INTEGER NOT NULL, messages INTEGER NOT NULL,
  body_versions INTEGER NOT NULL, attachments INTEGER NOT NULL,
  blobs INTEGER NOT NULL, blob_bytes INTEGER NOT NULL,
  computed_at_ns  INTEGER NOT NULL
) STRICT;

CREATE TABLE verify_run (
  id              INTEGER PRIMARY KEY,
  started_at_ns   INTEGER NOT NULL,
  ended_at_ns     INTEGER,
  depth           INTEGER NOT NULL CHECK (depth IN (0, 1)),   -- 0 quick, 1 full
  outcome         INTEGER,                                   -- 0 ok, 1 problems, 2 cancelled, 3 incomplete
  counts          TEXT                                       -- JSON: VerifyCounts (content-free)
) STRICT;
