OpenMessage Archive export

This folder is an export from an OpenMessage Archive archive: copies of messages, and of attachment files when they were included, from iPhone backups. The files are plain text and ordinary files. Anyone who can open this folder can read them; they are not encrypted.

To check that no file changed since the export: SHA256SUMS lists a SHA-256 checksum for every file, manifest.json included. On Linux, run 'sha256sum -c SHA256SUMS' in this folder. On Windows, compare each file's 'Get-FileHash <file>' output with its line. manifest.json lists every file except SHA256SUMS with its size and checksum.

manifest.json records what was exported: the scope kind, the formats, the options, the time zone table the times were written in, the counts and the kinds of data that are never exported. It never records a search's words, device facts, the archive's name or id, or any path.

Values marked 'derived' were worked out by the archive from what the phone recorded, not recorded by the phone itself. Text in brackets such as [withheld] or [masked] is a placeholder written by the export, not message text. Characters that do not show when printed (zero-width, direction and other invisible characters) are written as a code such as ⟨U+200B⟩ when markers are on. The character ⟨ itself is always escaped inside message text, so a placeholder can never be forged by a message.

One-time codes are masked only when the phone marked the message as holding one; other codes in message text are not detected. Text captured before a message was unsent or edited, and the attachments of unsent parts, are withheld unless the export was made with that option, after confirming the count.

Items are in the order of their recorded time. When two items have the same recorded time, their order is the order the archive imported them in, and they are flagged as a tie: that order is not something the phone recorded. Items without a usable recorded time come after the dated ones.

Conversations, messages, people's addresses and attachment files are numbered within this export only (1, 2, 3 ...). The numbers do not match the archive's own row numbers and say nothing about the archive's size; two exports cannot be matched on them.

CSV files (csv/) are UTF-8, comma-separated, with CRLF record ends and every field in quotes (RFC 4180, whose text is ASCII: UTF-8 is an extension). Read columns by their header name: the column contract is 'tabularColumnsVersion' in manifest.json, and columns are only ever added. Every file is present, with its header row only when an option left its rows out. In the 'for spreadsheets' profile each file starts with a byte order mark, control characters are written as codes such as ⟨U+001B⟩, and a cell that a spreadsheet could run as a formula starts with an apostrophe; the row's 'neutralised_columns' names the columns where that was done. In the 'for data tools' profile values are raw, except that ⟨ is escaped. A spreadsheet may round long numbers such as raw times or cut very long cells: the JSON files keep exact values.

The JSON files (json/) keep values as recorded, with codes instead of sentences, and follow the schema in schema/export-v1.schema.json, named by 'schemaVersion'. Each conversation is a JSON Lines file, one record per line; conversations.json and handles.json list the conversations and addresses. Numbers that can be too large for some readers, such as raw times, are written as strings. Line and paragraph separators and other characters that some tools treat as line ends are written as \u escapes. 'Raw' there means as stored in the archive, which is not always the bytes the phone stored: bytes that were not valid text show as U+FFFD and are flagged, and text is not normalised. The files are JSON (RFC 8259), not I-JSON.

Columns and keys are only ever added in a minor version; a key is never removed or renamed within a major version. The versions are in manifest.json.
