// Every word the viewer shows for a closed code (M5 §8.2). The viewer DTOs
// carry content only as `UntrustedText` (rendered by `SafeText`); every
// state, reason, label and kind is a closed code whose sentence is written
// here, in an exhaustive `Record<Code, string>`, so a new code fails
// `npm run typecheck` until it has words (the `importText.ts` pattern).
//
// Rules the sentences follow (I5, D94):
// - No read, delivery or sent state, in any form: there is no such code,
//   and `tests/frontend/viewer-text.spec.ts` asserts no key or sentence
//   names one.
// - A derived label says it is derived; an undeterminable value says so.
// - Nothing is called "not edited", "deleted on the phone" or "group".
import type {
  ConversationSortInput,
  AssocKindDto,
  AssocTargetShapeDto,
  AssocVisibilityDto,
  AttributedOutcomeDto,
  BlobVerifyStateDto,
  CardStateDto,
  ChatLabelDto,
  ChipSenderDto,
  ChipStateDto,
  ContainerRefusalDto,
  ContentKindDto,
  CopyDto,
  DirectionDto,
  DisplayNoneReasonDto,
  DisplaySourceDto,
  EditEntryTextDto,
  EditLabelDto,
  EditTextsOutcomeDto,
  ErrorCode,
  FileProblemDto,
  FocusReasonDto,
  FormattingDto,
  HistoryColumnDto,
  HitVersionDto,
  IdentifierProbeDto,
  ItemKindDto,
  LookupDto,
  MaskKindDto,
  MediaClassDto,
  MediaPlayback,
  MessageLabelDto,
  MotionChoice,
  NoHighlightDto,
  PageRangeDto,
  PathClassDto,
  PlaybackDto,
  PreviewBodyDto,
  ReactionKindDto,
  ReplyRefDto,
  ScanStopDto,
  SearchRanDto,
  SenderDto,
  ServiceClassDto,
  SniffedKindDto,
  SummaryTimeKindDto,
  TimeFormat,
  TimeUnitDto,
  TimeZoneChoice,
  TsErrorDto,
  ValueStateDto,
  ViewerSessionStateDto,
  WithheldReasonDto,
  WordsPlanDto,
} from '@/types';

type KindOf<T extends { kind: string }> = T['kind'];

// ------------------------------------------------------------- values

/** Why a content value (text, address, name) is missing. */
export const VALUE_STATE_TEXT: Record<ValueStateDto, string> = {
  null: 'Not recorded (NULL)',
  absent: 'Column not in this backup',
  oversized: 'Too large to store',
  unreadable: 'Could not be read',
};

/** Badges on a content value. */
export const CONTENT_BADGE_TEXT = {
  truncated: 'truncated',
  lossy: 'Some characters could not be decoded',
  storedAsBlob: 'Stored as raw bytes in the source',
  hidden: 'Contains invisible or direction-control characters',
} as const;

// ------------------------------------------------------------- time

/** A date the viewer cannot place on a calendar (§8.5). `{raw}` is replaced. */
export const TIME_UNIT_TEXT: Record<TimeUnitDto, string> = {
  zero: 'Date recorded as 0',
  negative: 'Negative date (raw {raw})',
  seconds: 'Recorded in seconds',
  nanoseconds: 'Recorded in nanoseconds',
  ambiguous: 'Date could not be interpreted (raw {raw})',
};

/** A date that is missing (§8.5). */
export const TIME_STATE_TEXT: Record<ValueStateDto, string> = {
  null: 'No date recorded (NULL)',
  absent: 'Date column not in this backup',
  oversized: 'Date too large to store',
  unreadable: 'Date could not be read',
};

export const BEFORE_2007_TEXT =
  'Before 2007, outside the range this app expects; it may not be a real date';

export const TIME_ZONE_NOTE =
  "Shown in this computer's time zone. No time zone is recorded in this archive.";
export const TIME_ZONE_NOTE_UTC = 'Shown in UTC. No time zone is recorded in this archive.';

export const SUMMARY_TIME_KIND_TEXT: Record<SummaryTimeKindDto, string> = {
  integer: 'time as recorded (whole number; unit assumed)',
  real: 'time as recorded (decimal number; unit assumed)',
  date: 'time as recorded (date value; unit assumed)',
};

// ------------------------------------------------------------- items

export const ITEM_KIND_TEXT: Record<ItemKindDto, string> = {
  unknown: 'Unrecognised item',
  message: 'Message',
  participantAdded: 'An address was added to the conversation',
  ownNumberChanged: 'The phone’s own number changed in the conversation',
  participantRemoved: 'An address was removed from the conversation',
  groupRenamed: 'The conversation name was changed',
  participantLeft: 'An address left the conversation',
  groupPhotoChanged: 'The conversation photo was changed',
  groupPhotoRemoved: 'The conversation photo was removed',
  backgroundChanged: 'The conversation background was changed',
  backgroundRemoved: 'The conversation background was removed',
  legacySharedLocation: 'Location sharing (older format, not decoded)',
  audioKept: 'An audio message was kept',
  sharePlay: 'SharePlay activity (not decoded)',
};

export const DIRECTION_TEXT: Record<DirectionDto, string> = {
  you: 'You',
  received: 'Received',
  notRecorded: 'Direction not recorded',
};

export const DIRECTION_DETAIL_TEXT: Record<DirectionDto, string> = {
  you: "Sent from the backed-up phone's account",
  received: 'Received by the backed-up phone',
  notRecorded: 'The source does not record the direction of this message',
};

export const SENDER_TEXT: Record<KindOf<SenderDto>, string> = {
  fromMe: 'You',
  resolved: 'Sender',
  noSender: 'No sender recorded',
  columnAbsent: 'Sender column not in this backup',
  unreadable: 'Sender could not be read',
  notFound: 'Sender recorded but not found in this archive',
  directionNotRecorded: 'Sender not shown: direction not recorded',
};

export const SERVICE_TEXT: Record<ServiceClassDto, string> = {
  iMessage: 'iMessage',
  sms: 'SMS',
  rcs: 'RCS',
  iMessageLite: 'iMessageLite',
  empty: 'No service recorded',
  other: 'Other service',
};

export const MESSAGE_LABEL_TEXT: Record<KindOf<MessageLabelDto>, string> = {
  undated: 'Undated (derived): no usable date is recorded',
  unkeyed: 'No unique identifier in its source; it may appear more than once',
  scheduleType:
    'schedule_type {value} recorded. For scheduled messages, the time shown is the planned send time',
  appContent: 'App or link content (not decoded)',
  effect: 'Sent with an effect',
  notInLatest: 'Not seen by the newest complete import of this backup set (derived)',
  provisional: 'Provisional (untested iOS version)',
};

// ------------------------------------------------------------- bodies

export const DISPLAY_NONE_TEXT: Record<DisplayNoneReasonDto, string> = {
  attributedBodyUnusable: 'The formatted text could not be used',
  textEmpty: 'The message text is empty',
  noSource: 'No text is recorded for this message',
};

export const ATTRIBUTED_OUTCOME_TEXT: Record<AttributedOutcomeDto, string> = {
  absent: 'The formatted text column is not in this backup',
  null: 'No formatted text recorded',
  oversized: 'The formatted text was larger than the import limit and was not stored',
  unreadable: 'The formatted text could not be read',
  notDecoded: 'The formatted text was not decoded',
  decoded: 'The formatted text was decoded',
  decodeFailed: 'The formatted text could not be decoded',
};

export const DISPLAY_SOURCE_TEXT: Record<DisplaySourceDto, string> = {
  attributedBody: 'From the formatted text',
  text: 'From the plain-text column',
  textLossy: 'Some characters could not be decoded',
};

export const SOURCES_DIFFER_TEXT =
  'The two stored text fields differ; which one the phone showed is not known';

export const FORMATTING_TEXT: Record<KindOf<FormattingDto>, string> = {
  plain: 'No formatting recorded',
  applied: 'Formatting applied',
  notApplied: 'Formatting not applied',
  unreadable: 'Formatting could not be read',
  tooManySegments: 'Too much formatting to show; shown as plain text',
};

export const TS_ERROR_TEXT: Record<TsErrorDto, string> = {
  unsupportedHeader: 'The formatted text uses a header this app does not read',
  truncated: 'The formatted text ends early',
  unexpectedTag: 'The formatted text contains an unexpected value',
  badReference: 'The formatted text refers to a value that is not there',
  unsupportedType: 'The formatted text uses a type this app does not read',
  tooDeep: 'The formatted text is nested too deeply',
  budget: 'The formatted text is too large to decode',
  materialisationBudget: 'The formatted text expands too much to decode',
  trailingBytes: 'The formatted text has unexpected data after its end',
  notAttributedString: 'The formatted text is not in the expected form',
  invalidUtf8: 'The formatted text contains invalid characters',
  runLengthMismatch: 'The formatting ranges do not match the text',
  runOverflow: 'The formatting ranges run past the text',
  runSplitsSurrogate: 'A formatting range splits a character',
  badRunIndex: 'A formatting range refers to a missing entry',
  unrecognised: 'The formatted text could not be decoded',
};

export const MASK_TEXT: Record<MaskKindDto, string> = {
  oneTimeCode: 'One-time code hidden',
};

export const SEGMENT_TEXT = {
  inlineEmoji: 'Inline emoji image (not shown)',
  unmatchedPlaceholder: 'Attachment placeholder not matched to a file',
  transcription: 'Transcription recorded on the phone',
  showCode: 'Show code',
  showMore: 'Show more',
} as const;

// ------------------------------------------------------------- edits

/** Codes 0, 2, 9 never read "not edited"; codes 6, 7 never read "unsent". */
export const EDIT_LABEL_TEXT: Record<EditLabelDto, string> = {
  undeterminable: 'Edit state could not be determined',
  notRecorded: 'No edit information recorded',
  summaryUnreadable: 'Edit state could not be determined',
  unsentPartsAndEditHistory: 'Edited and partly unsent (derived)',
  unsentParts: 'Unsent by the sender (derived)',
  editHistory: 'Edited (derived)',
  dateEditedNoBody: 'An edit date is recorded; no edit history in this version',
  dateEditedOnly: 'An edit date is recorded; no edit history in this version',
  noEditRecorded: 'No edit recorded',
  undetermined: 'Edit state could not be determined',
};

export const EDIT_TEXTS_OUTCOME_TEXT: Record<EditTextsOutcomeDto, string> = {
  notNeeded: 'No recorded edit text',
  decoded: 'Edit text decoded from the stored record',
  unmatched: 'Edit text could not be matched',
  summaryUnreadable: 'The edit record could not be read',
};

export const EDIT_ENTRY_TEXT: Record<KindOf<EditEntryTextDto>, string> = {
  text: 'Text recorded for this version',
  withheld: 'Earlier text withheld',
  noBody: 'No text recorded for this version',
  failed: 'Edit text could not be decoded',
  notMatched: 'Edit text could not be matched',
};

/** The sentence before the "Show earlier text" button (D115). */
export const WITHHELD_TEXT: Record<WithheldReasonDto, string> = {
  unsent: 'Earlier text was kept from an import before it was unsent.',
  earlierVersion: 'Earlier text was kept from an import.',
  noCurrentText: 'Earlier text was kept from an import.',
};

export const REVEALED_TEXT: Record<WithheldReasonDto, string> = {
  unsent:
    'This text is from an import taken before it was unsent; the phone no longer shows it',
  earlierVersion: 'This text is from an earlier version kept by an import',
  noCurrentText: 'This text is from an earlier version kept by an import',
};

export const EDIT_UI_TEXT = {
  edited: 'Edited',
  unsentPart: 'Unsent by the sender',
  storedVersions: '{n} stored versions',
  partsUnsent: '{n} of {total} parts unsent',
  versionsCaptured: 'Versions this archive captured',
  versionsOnPhone: 'Versions recorded on the phone',
  textUnchanged: 'Stored data changed; text unchanged',
  searchCovers: 'Search covers captured versions only',
} as const;

// ------------------------------------------------------------- reactions

export const ASSOC_KIND_TEXT: Record<AssocKindDto, string> = {
  noAssociation: 'Not a reaction',
  stickerLegacy: 'Sticker placed on a message',
  pollVote: 'Poll vote (poll contents not decoded)',
  unknown: 'Unrecognised association',
  tapbackLove: 'Loved',
  tapbackLike: 'Liked',
  tapbackDislike: 'Disliked',
  tapbackLaugh: 'Laughed at',
  tapbackEmphasize: 'Emphasised',
  tapbackQuestion: 'Questioned',
  tapbackEmoji: 'Reacted with an emoji',
  tapbackSticker: 'Sticker reaction',
  removedLove: 'Removed a heart',
  removedLike: 'Removed a like',
  removedDislike: 'Removed a dislike',
  removedLaugh: 'Removed a laugh',
  removedEmphasize: 'Removed an emphasis',
  removedQuestion: 'Removed a question',
  removedEmoji: 'Removed an emoji reaction',
  removedSticker: 'Removed a sticker reaction',
};

export const ASSOC_SHAPE_TEXT: Record<AssocTargetShapeDto, string> = {
  part: 'Target recorded with a part number',
  bubble: 'Target recorded as a whole bubble',
  bare: 'Target recorded in a form this app has not verified',
  unparsed: 'Target could not be parsed',
};

export const ASSOC_VISIBILITY_TEXT: Record<KindOf<AssocVisibilityDto>, string> = {
  undeterminable: 'Whether this folds into its target could not be determined',
  noKind: 'Reaction kind not recorded',
  notAReaction: 'Not a reaction',
  targetUnreadable: 'Reaction target could not be read',
  targetNotUnique: 'Reaction target is not uniquely identified',
  targetInAnotherChat: 'Reaction to a message in another conversation',
  folded: 'Shown on the message it reacts to',
  targetFolds: 'Reaction to another reaction, which is not shown as an item of its own',
};

export const REACTION_KIND_TEXT: Record<ReactionKindDto, string> = {
  love: 'Loved',
  like: 'Liked',
  dislike: 'Disliked',
  laugh: 'Laughed',
  emphasize: 'Emphasised',
  question: 'Questioned',
  emoji: 'Emoji reaction',
  sticker: 'Sticker reaction',
};

export const CHIP_STATE_TEXT: Record<ChipStateDto, string> = {
  shown: 'recorded (derived)',
  hidden: 'removed later (derived)',
  undeterminable: 'cannot be determined',
  added: 'recorded add',
  removed: 'recorded removal',
};

export const CHIP_SENDER_TEXT: Record<KindOf<ChipSenderDto>, string> = {
  me: 'You',
  handle: 'an address',
  notRecorded: 'sender not recorded',
};

export const REACTION_RULE_TEXT =
  'Chips are derived from the recorded rows: the latest dated row per sender and kind wins, and a removal hides the chip. Undated or tied rows make the state "cannot be determined".';

// ------------------------------------------------------------- replies

export const REPLY_TEXT: Record<KindOf<ReplyRefDto>, string> = {
  resolved: 'Reply to',
  inAnotherConversation: 'Reply to a message in another conversation',
  notUniquelyIdentified: 'No uniquely identified message with this identifier is in the archive',
};

export const PREVIEW_BODY_TEXT: Record<KindOf<PreviewBodyDto>, string> = {
  text: '',
  noText: 'No text',
  noBody: 'No text recorded',
  event: 'Conversation event',
  reaction: 'Reaction',
  hidden: 'Text not shown',
};

export const IDENTIFIER_PROBE_TEXT: Record<IdentifierProbeDto, string> = {
  notInArchive: 'No message with this identifier is in the archive',
  sharedByUnkeyed: 'Unkeyed messages share this identifier',
  notChecked: 'Not checked',
};

// ------------------------------------------------------------- conversations

export const CHAT_LABEL_TEXT: Record<KindOf<ChatLabelDto>, string> = {
  name: '',
  nameUnreadable: 'Name recorded but too large or unreadable',
  participants: '(from participants)',
  identifier: '',
  unnamed: 'Conversation without a name or participants',
};

export const HISTORY_COLUMN_TEXT: Record<HistoryColumnDto, string> = {
  subject: 'subject',
  groupTitle: 'group_title',
  destinationCallerId: 'destination_caller_id (Account, meaning unverified)',
  chatDisplayName: 'display_name',
  transferName: 'transfer_name',
  emojiDescription: 'emoji_description',
};

export const IMPORT_STATE_TEXT: Record<ViewerSessionStateDto, string> = {
  running: 'running',
  completed: 'completed',
  cancelled: 'cancelled',
  failed: 'failed',
  interrupted: 'interrupted',
  superseded: 'superseded',
  stopped: 'stopped',
};

export const PAGE_RANGE_TEXT: Record<PageRangeDto, string> = {
  dated: 'Dated items',
  undated: 'Undated items',
  replies: 'Replies',
};

export const FOCUS_REASON_TEXT: Record<FocusReasonDto, string> = {
  anchor: 'Showing the message you chose',
  reactionTarget: 'Showing the message this reaction is shown on',
  nextVisible: 'That item is shown on another message; showing the nearest visible item',
};

// ------------------------------------------------------------- attachments

export const SNIFFED_KIND_TEXT: Record<SniffedKindDto, string> = {
  gif: 'GIF image',
  webp: 'WebP image',
  png: 'PNG image',
  jpeg: 'JPEG image',
  bmp: 'BMP image',
  ico: 'Icon image',
  quickTime: 'QuickTime video',
  m4a: 'MPEG-4 audio',
  m4v: 'MPEG-4 video',
  heif: 'HEIC image: preview not available in this version',
  mp4: 'MPEG-4 video',
  isoBmffOther: 'MPEG-4 family file',
  webm: 'WebM video',
  wav: 'WAV audio',
  aiff: 'AIFF audio',
  ogg: 'Ogg audio',
  mp3: 'MP3 audio',
  caf: 'Core Audio file',
  pdf: 'PDF document',
  xml: 'XML document',
  html: 'HTML document (shown as a file only)',
  vcard: 'Contact card (vCard)',
  text: 'Text file',
  unknown: 'Unrecognised content',
};

export const MEDIA_CLASS_TEXT: Record<KindOf<MediaClassDto>, string> = {
  image: 'Image',
  playable: 'Audio or video',
  file: 'File',
  unrecognised: 'Unrecognised content',
};

export const CONTENT_KIND_TEXT: Record<KindOf<ContentKindDto>, string> = {
  noFile: 'No file is stored for this attachment',
  sniffed: 'Content',
  unavailable: 'The stored file cannot be used',
};

export const FILE_PROBLEM_TEXT: Record<FileProblemDto, string> = {
  notFound: 'The stored file was not found',
  badName: 'The stored file has an unexpected name',
  missing: 'The stored file is missing',
  notRegular: 'The stored file is not a regular file',
  lengthMismatch: 'The stored file has the wrong size',
  io: 'The stored file could not be read',
  digestMismatch: 'Preview unavailable: the file does not match its recorded checksum',
  changed: 'The stored file changed while it was being read',
  notVerified: 'The stored file has not been checked yet',
  overLimit: 'The stored file is too large to show',
};

export const CARD_TEXT: Record<KindOf<CardStateDto>, string> = {
  file: 'File',
  noObservation: 'No copy of this file was attempted by any import',
  notInBackup: 'This file was not in the backup',
  notFoundAtAssumedKey:
    'Not found where this app looked; it may be stored elsewhere in the backup',
  notCopied: 'Not copied: attachment copy was off for that import',
  classNotLookedUp: 'This kind of file location is not looked up by this version',
  locationUnusable: "The file's recorded location could not be used",
  notDecrypted: 'Listed in the backup but could not be decrypted',
  couldNotBeRead: 'Listed in the backup but could not be read',
  tooLarge: 'Too large to copy',
  unavailable: 'Not available (lookup code {lookup}, copy code {copy})',
};

export const OLDER_BACKUP_TEXT =
  'Showing the file from an older backup; not found in the newest backup';

export const BLOB_VERIFY_TEXT: Record<BlobVerifyStateDto, string> = {
  ok: 'The full verify found no problem',
  missing: 'The full verify reported: file missing',
  notRegular: 'The full verify reported: not a regular file',
  lengthMismatch: 'The full verify reported: wrong size',
  hashMismatch: 'The full verify reported: checksum mismatch',
  unreadable: 'The full verify reported: could not be read',
  badName: 'The full verify reported: unexpected name',
};

export const VERIFY_STALE_TEXT = 'This result may be out of date; run Verify';

export const LOOKUP_TEXT: Record<LookupDto, string> = {
  notLookedUp: 'Not looked up',
  notAttemptedNull: 'Not looked up: no location recorded',
  notAttemptedTooLong: 'Not looked up: the recorded location is too long',
  notAttemptedUnusualName: 'Not looked up: the recorded location has an unusual name',
  notAttemptedClass: 'Not looked up: this kind of location is not looked up by this version',
  notAttemptedUnrecognized: 'Not looked up: the recorded location is not recognised',
  notAttemptedColumnAbsent: 'Not looked up: the location column is not in this backup',
  notAttemptedUnreadable: 'Not looked up: the recorded location could not be read',
  notAttemptedOther: 'Not looked up (other reason)',
  present: 'Found in the backup',
  blobMissing: 'Listed, but the file is missing from the backup',
  blobNotRegular: 'Listed, but not a regular file in the backup',
  blobUnreadable: 'Listed, but could not be read',
  notAFile: 'Listed, but not a file',
  entryInvalid: 'Listed, but the entry is invalid',
  noKey: 'Listed, but no decryption key is recorded',
  noProtectionClass: 'Listed, but no protection class is recorded',
  classKeyUnavailable: 'Listed, but the protection class key is unavailable',
  keyDoesNotUnwrap: 'Listed, but the key could not be unwrapped',
  sizeMismatch: 'Listed, but the size does not match',
  notProbed: 'Not probed',
  probeOther: 'Probe failed (other reason)',
  notListedAtSourcedKey: 'Not in the backup',
  notFoundAtAssumedKey: 'Not found where this app looked',
  ambiguous: 'More than one matching entry',
  invalidEntry: 'Invalid entry',
  lookupOther: 'Lookup failed (other reason)',
};

export const COPY_TEXT: Record<CopyDto, string> = {
  copied: 'Copied',
  deduplicated: 'Same file already stored',
  adopted: 'An existing stored file was used',
  orphanReplaced: 'An unused stored file was replaced',
  alreadyArchived: 'Already in the archive',
  notRequested: 'Not copied: attachment copy was off',
  notLookedUp: 'Not looked up',
  notInBackup: 'Not in the backup',
  blobUnavailable: 'Could not be read from the backup',
  storeConflict: 'Could not be stored (conflict)',
  overLimit: 'Too large to copy',
  tooLargeForVolume: 'Too large for the archive volume',
  readFailed: 'Reading failed',
  writeFailed: 'Writing failed',
  replaced: 'Replaced by a later import',
};

export const PATH_CLASS_TEXT: Record<PathClassDto, string> = {
  noClass: 'No location class',
  smsAttachments: 'Messages attachments folder',
  smsStickerCache: 'Sticker cache',
  macMessages: 'Mac Messages folder',
  otherHomeRelative: 'Another folder in the home directory',
  absoluteVarMobile: 'An absolute phone path',
  absolutePrivateVarMobile: 'An absolute private phone path',
  unrecognized: 'Unrecognised location',
  other: 'Other location',
};

export const CONTAINER_REFUSAL_TEXT: Record<ContainerRefusalDto, string> = {
  tooDeep: 'nested too deeply',
  tooManyBoxes: 'too many parts',
  boxPastEnd: 'a part runs past the end of the file',
  sizeOverflow: 'a part size is out of range',
  sizeZeroNested: 'a nested part has no size',
  boxTooSmall: 'a part is too small',
  referenceMovie: 'it refers to another file or address',
  externalDataReference: 'it refers to data outside the file',
  noMoov: 'no movie header',
  chunkPastEnd: 'a chunk runs past the end of the file',
  dataBeforeFmt: 'audio data before its format',
  noFmt: 'no audio format',
  timedOut: 'the check took too long',
  notPlayable: 'not a playable format',
};

export const PLAYBACK_TEXT: Record<KindOf<PlaybackDto>, string> = {
  allowed: 'Ready to play',
  refused: "This file can't be played in the app: its structure failed a safety check",
  notPlayable: "This format can't be played in the app",
  off: 'Playback is turned off in Settings',
  unavailable: 'The stored file cannot be used',
};

export const MEDIA_UI_TEXT = {
  checking: 'Checking the file…',
  cannotPlayHere: "This format can't be played in the app",
  previewsOff: 'Image previews are off for this session after repeated failures',
  rendition: 'Preview, re-encoded by this app',
  declared: 'declared',
  sticker: 'Sticker',
  confirm:
    "This file will be played by your system's media codecs (GStreamer on Linux, Media Foundation on Windows). A file crafted by its sender could exploit a flaw in them, and this app cannot make them safe. It has checked the file's structure. Play it?",
} as const;

// ------------------------------------------------------------- search

export const HIT_VERSION_TEXT: Record<HitVersionDto, string> = {
  current: 'Current text',
  earlier: 'Earlier version',
  unsent: 'Text from before it was unsent',
};

export const NO_HIGHLIGHT_TEXT: Record<NoHighlightDto, string> = {
  markerInText: 'Match not highlighted (the text contains marker characters)',
  mismatch: 'Match not highlighted',
  budget: 'Match not highlighted (too costly)',
  notFound: 'Match not highlighted',
};

export const SCAN_STOP_TEXT: Record<ScanStopDto, string> = {
  examinedLimit: 'The scan stopped after examining its limit of messages',
  matchCap: 'The scan stopped at its limit of matches',
  tooBroad: 'The scan stopped: the search was too broad',
};

export const SEARCH_RAN_TEXT: Record<SearchRanDto, string> = {
  words: 'Words',
  exact: 'Exact text',
};

export const WORDS_PLAN_TEXT: Record<WordsPlanDto, string> = {
  global: 'All conversations',
  scoped: 'This conversation',
};

// ------------------------------------------------------------- errors

type ViewerErrorCode = Extract<
  ErrorCode,
  | 'viewerClosed'
  | 'queryTooBroad'
  | 'invalidQuery'
  | 'searchUnavailable'
  | 'searchSuperseded'
  | 'messageNotFound'
  | 'anchorNotFound'
  | 'attachmentUnavailable'
>;

export const VIEWER_ERROR_TEXT: Record<ViewerErrorCode, string> = {
  viewerClosed: 'The viewer was closed. Open the archive again.',
  queryTooBroad: 'This request covers too much at once. Narrow it and try again.',
  invalidQuery: 'This search could not be understood.',
  searchUnavailable: 'Search is not available for this archive.',
  searchSuperseded: '',
  messageNotFound: 'That message is no longer in the archive.',
  anchorNotFound: 'That position is no longer in the conversation.',
  attachmentUnavailable: 'That attachment is not available.',
};

export const VIEWER_STATE_TEXT = {
  skippedRows: 'Some rows have identifiers this viewer cannot show; run Verify',
  extrasIncomplete: 'Reactions and replies could not all be read for this page',
  partial: 'Only part of this page could be read in time',
  anchorMoved: 'An import moved this position. The page was reloaded around the same message.',
  importRunning: 'An import is running. Newly imported messages appear when you reload.',
  emptyChat: 'No messages for this conversation are in the archive',
  noConversations: 'This archive has no conversations',
  pageFailed: 'This page could not be loaded.',
  locationBusy: 'Close the viewer to change the archive location',
  showingWindow: 'Showing items around {from} – {to}',
} as const;

// ------------------------------------------------------------- list and thread (5h1)

/** The conversation list. `{n}` and `{total}` are counts. */
export const LIST_TEXT = {
  label: 'Conversations',
  filterLabel: 'Filter conversations by name or address',
  filterHint: 'Names and addresses only; message text is searched with Search.',
  sortLabel: 'Order',
  loadingRow: 'Loading…',
  itemsRecorded: '{n} items recorded',
  itemsRecordedCapped: '{n}+ items recorded',
  statsStale: 'count may be out of date',
  noStats: 'Item count not available',
  lastDated: 'Last dated item {date}',
  noDated: 'No dated items',
  undated: '{n} undated',
  undatedCapped: '{n}+ undated',
  group: 'Group',
  empty: 'No messages in the archive',
  truncated: 'Showing the first {n} of {total} conversations. Narrow the filter to see the rest.',
  noMatch: 'No conversation matches this filter',
  previewYou: 'You:',
  unnamedId: 'Conversation without a name or participants (#{id})',
  moreParticipants: '+{n}',
} as const;

export const SORT_TEXT: Record<ConversationSortInput, string> = {
  recent: 'Most recent dated activity',
  name: 'Name',
  count: 'Most items',
};

/** The thread. `{n}` is a count, `{date}` a formatted date. */
export const THREAD_TEXT = {
  addresses: '{n} addresses recorded in this conversation',
  oneAddress: '1 address recorded in this conversation',
  itemsRecorded: '{n} items recorded',
  dateRange: '{first} – {last}',
  jumpFirst: 'Jump to first',
  jumpNewest: 'Jump to newest',
  undatedButton: 'Undated items ({n})',
  undatedButtonCapped: 'Undated items ({n}+)',
  undatedHeading: 'Undated items, in source order',
  backToDated: 'Back to dated items',
  loadingOlder: 'Loading earlier items…',
  loadingNewer: 'Loading later items…',
  loadingFirst: 'Loading…',
  retry: 'Retry',
  cutForSize: 'This page was cut short to stay within its size limit; scroll to load the rest',
  chooseConversation: 'Choose a conversation',
  chooseHint: 'Pick one from the list to read it here.',
  senderNotLoaded: 'Sender (address not loaded with this page)',
  attachments: '{n} attachments',
  oneAttachment: '1 attachment',
  attachmentInText: 'Attachment',
  feedLabel: 'Messages',
  closeViewer: 'Close viewer',
  opening: 'Opening the archive…',
  infoHeading: 'This archive in the viewer',
  infoChats: '{n} conversations',
  infoMessages: '{n} messages',
  infoAttachments: '{n} attachments',
  infoNoStats: 'Totals are not available until an import completes',
  provisional: 'The newest import is not complete; derived values are provisional',
  unsearchable: '{n} messages have no searchable text',
  sessionClosed: 'The viewer is closed.',
} as const;

// ------------------------------------------------------------- bubbles (5h2)

/**
 * Message bubbles and system rows (§3.6, §8.4). `{n}`, `{total}`,
 * `{parts}`, `{value}` and `{lineage}` are counts, raw recorded integers
 * or ids; `{date}` is a formatted date.
 */
export const BUBBLE_TEXT = {
  edited: 'Edited',
  editedParts: 'Edited (recorded part {parts})',
  editedUnknownParts: 'An edit is recorded; which parts is not recorded',
  unsentWhole: 'Unsent by the sender',
  partsUnsent: '{n} of {total} parts unsent by the sender',
  unsentParts: 'Unsent by the sender: recorded part {parts}',
  unsentUnknownParts: 'An unsend is recorded; which parts is not recorded',
  storedVersions: '{n} stored versions',
  showEarlier: 'Show earlier text',
  hideEarlier: 'Hide earlier text',
  revealedFrom: 'First kept by the import of {date}',
  showCode: 'Show code',
  hideCode: 'Hide code',
  revealFailed: 'The text could not be shown.',
  showFullText: 'Show the full text',
  showMoreText: 'Show more of the text',
  showMore: 'Show more',
  showLess: 'Show less',
  fullTextWithheld: 'The rest of this text is withheld.',
  fullTextNone: 'No more text is recorded for this message.',
  loadingText: 'Loading the text…',
  textColumn: 'Plain-text column: {state}',
  urlHidden: 'This link contains hidden characters',
  directionRecordedBy: 'As recorded by backup set {lineage}; another phone’s backup may disagree',
  received: 'Received',
  recentlyDeleted: 'Listed in Recently Deleted in the backup imported on {date}',
  recentlyDeletedUnknown: 'Listed in Recently Deleted in a backup this archive imported',
  notListedLater: 'Not listed in the backup imported on {date}',
  notListedLaterUnknown: 'Not listed in a later backup of the same set',
  extrasMissing: 'Reactions and replies could not be read for this message',
  replies: '{n} replies across conversations',
  repliesCapped: '{n}+ replies across conversations',
  oneReply: '1 reply across conversations',
  replyJump: 'Show the message this replies to',
  replyOtherChat: 'Open that conversation at the message',
  reactionsHeading: 'Reactions',
  reactionBy: 'by',
  reactionsOver: 'More than {n} reactions recorded; too many to show here',
  reactionsMore: '{n} more reactions not shown',
  reactionsRemoved: '{n} removed later (derived)',
  oneReactionRemoved: '1 removed later (derived)',
  emojiRemovalUnpaired: 'Removal of an emoji reaction (emoji not recorded)',
  emojiRemovalUnpairedMany: 'Removal of an emoji reaction (emoji not recorded), {n} rows',
  reactionKindNotRecorded: 'Reaction kind not recorded',
  unknownItem: 'Unrecognised item (item_type {item}, group_action_type {action})',
  rawNotRecorded: 'not recorded',
  itemKindNotRecognised: 'Item of a kind this version does not recognise',
  eventAddresses: 'Addresses recorded with this event:',
  eventNoAddresses: 'No address is recorded with this event',
  addressNotLoaded: 'address not loaded with this page',
  recordedText: 'Text recorded with this item:',
  newTitle: 'Name recorded:',
  serviceChanged: 'Service: {service}',
  serviceNotRecognised: 'Service not recognised by this version',
} as const;

// ------------------------------------------------------------- settings

export const TIME_FORMAT_TEXT: Record<TimeFormat, string> = {
  system: 'System default',
  h12: '12-hour',
  h23: '24-hour',
};

export const TIME_ZONE_TEXT: Record<TimeZoneChoice, string> = {
  system: "This computer's time zone",
  utc: 'UTC',
};

export const MOTION_TEXT: Record<MotionChoice, string> = {
  system: 'Follow the system setting',
  reduce: 'Always reduce motion',
};

export const MEDIA_PLAYBACK_TEXT: Record<MediaPlayback, string> = {
  off: 'Off: audio and video are never played',
  confirm: 'Ask before playing each file (recommended)',
  clickToPlay: 'Play when I click Play, without asking',
};

/** Replace `{name}` placeholders with closed values (numbers or codes). */
export function fill(template: string, values: Record<string, string | number>): string {
  return template.replace(/\{(\w+)\}/g, (whole, key: string) =>
    key in values ? String(values[key]) : whole,
  );
}
