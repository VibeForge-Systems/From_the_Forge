//! Attribute keys (M3 §10.8).
//!
//! The key names are facts from the T6 source (imessage-database 4.3.0's
//! documentation of `body.rs`), recorded in the M3 research; no code was
//! taken from it. The layouts of the link, data-detector, money, address
//! and calendar values are UNVERIFIED, so those keys are recorded as
//! present and nothing more.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

/// A known attribute key. The set is closed: any other key is counted as
/// unknown and its name is never kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttrKey {
    /// `__kIMMessagePartAttributeName`: the message part index (`NSNumber`).
    MessagePart,
    /// `__kIMFileTransferGUIDAttributeName`: an attachment's transfer GUID.
    FileTransferGuid,
    /// `__kIMFilenameAttributeName`.
    Filename,
    /// `__kIMInlineMediaWidthAttributeName`.
    InlineMediaWidth,
    /// `__kIMInlineMediaHeightAttributeName`.
    InlineMediaHeight,
    /// `IMAudioTranscription`.
    AudioTranscription,
    /// `__kIMMentionConfirmedMention`: a mentioned handle.
    Mention,
    /// `__kIMLinkAttributeName` (value layout UNVERIFIED).
    Link,
    /// `__kIMEmojiImageAttributeName`: an inline emoji image.
    EmojiImage,
    /// `__kIMTextEffectAttributeName`: an effect number.
    TextEffect,
    /// `__kIMTextBoldAttributeName`.
    Bold,
    /// `__kIMTextItalicAttributeName`.
    Italic,
    /// `__kIMTextUnderlineAttributeName`.
    Underline,
    /// `__kIMTextStrikethroughAttributeName`.
    Strikethrough,
    /// `__kIMOneTimeCodeAttributeName`.
    OneTimeCode,
    /// `__kIMCalendarEventAttributeName` (value layout UNVERIFIED).
    CalendarEvent,
    /// `__kIMDataDetectedAttributeName` (value layout UNVERIFIED).
    DataDetected,
    /// `__kIMMoneyAttributeName` (value layout UNVERIFIED).
    Money,
    /// `__kIMAddressAttributeName` (value layout UNVERIFIED).
    Address,
}

impl AttrKey {
    /// Every key, in a fixed order (the order of [`AttrKeysPresent`]'s
    /// bits).
    pub const ALL: [AttrKey; 19] = [
        AttrKey::MessagePart,
        AttrKey::FileTransferGuid,
        AttrKey::Filename,
        AttrKey::InlineMediaWidth,
        AttrKey::InlineMediaHeight,
        AttrKey::AudioTranscription,
        AttrKey::Mention,
        AttrKey::Link,
        AttrKey::EmojiImage,
        AttrKey::TextEffect,
        AttrKey::Bold,
        AttrKey::Italic,
        AttrKey::Underline,
        AttrKey::Strikethrough,
        AttrKey::OneTimeCode,
        AttrKey::CalendarEvent,
        AttrKey::DataDetected,
        AttrKey::Money,
        AttrKey::Address,
    ];

    /// The key's name in the stream.
    pub const fn name(self) -> &'static str {
        match self {
            AttrKey::MessagePart => "__kIMMessagePartAttributeName",
            AttrKey::FileTransferGuid => "__kIMFileTransferGUIDAttributeName",
            AttrKey::Filename => "__kIMFilenameAttributeName",
            AttrKey::InlineMediaWidth => "__kIMInlineMediaWidthAttributeName",
            AttrKey::InlineMediaHeight => "__kIMInlineMediaHeightAttributeName",
            AttrKey::AudioTranscription => "IMAudioTranscription",
            AttrKey::Mention => "__kIMMentionConfirmedMention",
            AttrKey::Link => "__kIMLinkAttributeName",
            AttrKey::EmojiImage => "__kIMEmojiImageAttributeName",
            AttrKey::TextEffect => "__kIMTextEffectAttributeName",
            AttrKey::Bold => "__kIMTextBoldAttributeName",
            AttrKey::Italic => "__kIMTextItalicAttributeName",
            AttrKey::Underline => "__kIMTextUnderlineAttributeName",
            AttrKey::Strikethrough => "__kIMTextStrikethroughAttributeName",
            AttrKey::OneTimeCode => "__kIMOneTimeCodeAttributeName",
            AttrKey::CalendarEvent => "__kIMCalendarEventAttributeName",
            AttrKey::DataDetected => "__kIMDataDetectedAttributeName",
            AttrKey::Money => "__kIMMoneyAttributeName",
            AttrKey::Address => "__kIMAddressAttributeName",
        }
    }

    /// The key a name denotes, if it is known.
    pub fn from_name(name: &[u8]) -> Option<AttrKey> {
        AttrKey::ALL
            .into_iter()
            .find(|k| k.name().as_bytes() == name)
    }

    fn bit(self) -> u32 {
        let i = AttrKey::ALL
            .iter()
            .position(|k| *k == self)
            .and_then(|i| u32::try_from(i).ok())
            .unwrap_or(31);
        1u32.checked_shl(i).unwrap_or(0)
    }

    /// The flag this key sets when its value is a non-zero `NSNumber`.
    pub(crate) fn flag(self) -> Option<AttrFlag> {
        match self {
            AttrKey::Bold => Some(AttrFlag::Bold),
            AttrKey::Italic => Some(AttrFlag::Italic),
            AttrKey::Underline => Some(AttrFlag::Underline),
            AttrKey::Strikethrough => Some(AttrFlag::Strikethrough),
            AttrKey::OneTimeCode => Some(AttrFlag::OneTimeCode),
            AttrKey::EmojiImage => Some(AttrFlag::InlineEmoji),
            AttrKey::AudioTranscription => Some(AttrFlag::AudioTranscription),
            _ => None,
        }
    }
}

/// Which known keys a run's dictionary holds, whatever their values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct AttrKeysPresent(u32);

impl AttrKeysPresent {
    /// `true` when the key is present.
    pub fn contains(self, key: AttrKey) -> bool {
        self.0 & key.bit() != 0
    }

    /// The present keys, in [`AttrKey::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = AttrKey> {
        AttrKey::ALL.into_iter().filter(move |k| self.contains(*k))
    }

    pub(crate) fn insert(&mut self, key: AttrKey) {
        self.0 |= key.bit();
    }

    pub(crate) fn union(self, other: AttrKeysPresent) -> AttrKeysPresent {
        AttrKeysPresent(self.0 | other.0)
    }
}

/// A formatting flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttrFlag {
    /// Bold.
    Bold,
    /// Italic.
    Italic,
    /// Underline.
    Underline,
    /// Strikethrough.
    Strikethrough,
    /// A one-time code.
    OneTimeCode,
    /// An inline emoji image.
    InlineEmoji,
    /// An audio transcription.
    AudioTranscription,
}

impl AttrFlag {
    /// Every flag, in a fixed order.
    pub const ALL: [AttrFlag; 7] = [
        AttrFlag::Bold,
        AttrFlag::Italic,
        AttrFlag::Underline,
        AttrFlag::Strikethrough,
        AttrFlag::OneTimeCode,
        AttrFlag::InlineEmoji,
        AttrFlag::AudioTranscription,
    ];

    /// A stable lower-case name.
    pub const fn name(self) -> &'static str {
        match self {
            AttrFlag::Bold => "bold",
            AttrFlag::Italic => "italic",
            AttrFlag::Underline => "underline",
            AttrFlag::Strikethrough => "strikethrough",
            AttrFlag::OneTimeCode => "one_time_code",
            AttrFlag::InlineEmoji => "inline_emoji",
            AttrFlag::AudioTranscription => "audio_transcription",
        }
    }

    fn bit(self) -> u8 {
        let i = AttrFlag::ALL
            .iter()
            .position(|f| *f == self)
            .and_then(|i| u32::try_from(i).ok())
            .unwrap_or(7);
        1u8.checked_shl(i).unwrap_or(0)
    }
}

/// Formatting flags, each set only from its attribute's **value**: an
/// `NSNumber` that is present and non-zero. A key that is present with any
/// other value sets nothing here; it is recorded in [`AttrKeysPresent`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct AttrFlags(u8);

impl AttrFlags {
    /// `true` when the flag is set.
    pub fn contains(self, flag: AttrFlag) -> bool {
        self.0 & flag.bit() != 0
    }

    /// The set flags, in [`AttrFlag::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = AttrFlag> {
        AttrFlag::ALL.into_iter().filter(move |f| self.contains(*f))
    }

    pub(crate) fn insert(&mut self, flag: AttrFlag) {
        self.0 |= flag.bit();
    }

    pub(crate) fn union(self, other: AttrFlags) -> AttrFlags {
        AttrFlags(self.0 | other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_bits_are_distinct() {
        let mut all = AttrKeysPresent::default();
        for k in AttrKey::ALL {
            assert_eq!(AttrKey::from_name(k.name().as_bytes()), Some(k));
            assert!(!all.contains(k));
            all.insert(k);
            assert!(all.contains(k));
        }
        assert_eq!(all.iter().count(), AttrKey::ALL.len());
        assert_eq!(AttrKey::from_name(b"__kIMSomethingElse"), None);
        let mut f = AttrFlags::default();
        for x in AttrFlag::ALL {
            assert!(!f.contains(x));
            f.insert(x);
        }
        assert_eq!(f.iter().count(), 7);
    }
}
