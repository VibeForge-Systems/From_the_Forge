OpenMessage Archive export

This folder is an export from an OpenMessage Archive archive: copies of messages, and of attachment files when they were included, from iPhone backups. The files are plain text and ordinary files. Anyone who can open this folder can read them; they are not encrypted.

To check that no file changed since the export: SHA256SUMS lists a SHA-256 checksum for every file, manifest.json included. On Linux, run 'sha256sum -c SHA256SUMS' in this folder. On Windows, compare each file's 'Get-FileHash <file>' output with its line. manifest.json lists every file except SHA256SUMS with its size and checksum.

manifest.json records what was exported: the scope kind, the formats, the options, the time zone table the times were written in, the counts and the kinds of data that are never exported. It never records a search's words, device facts, the archive's name or id, or any path.

Values marked 'derived' were worked out by the archive from what the phone recorded, not recorded by the phone itself. Text in brackets such as [withheld] or [masked] is a placeholder written by the export, not message text. Characters that do not show when printed (zero-width, direction and other invisible characters) are written as a code such as ⟨U+200B⟩ when markers are on. The character ⟨ itself is always escaped inside message text, so a placeholder can never be forged by a message.

One-time codes are masked only when the phone marked the message as holding one; other codes in message text are not detected. Text captured before a message was unsent or edited, and the attachments of unsent parts, are withheld unless the export was made with that option, after confirming the count.

Items are in the order of their recorded time. When two items have the same recorded time, their order is the order the archive imported them in, and they are flagged as a tie: that order is not something the phone recorded. Items without a usable recorded time come after the dated ones.

Conversations, messages, people's addresses and attachment files are numbered within this export only (1, 2, 3 ...). The numbers do not match the archive's own row numbers and say nothing about the archive's size; two exports cannot be matched on them.

Columns and keys are only ever added in a minor version; a key is never removed or renamed within a major version. The versions are in manifest.json.
