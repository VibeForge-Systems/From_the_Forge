//! Coded values, mapped only where a source documents the meaning (M3 §5.3).
//! Every record keeps the raw value next to any mapped one.
//!
//! This slice has the service classification only; the association, reaction
//! and item-kind codes land with the message records.

/// The class of a `service` or `service_name` value.
///
/// A case-insensitive ASCII match against `imessage`, `sms`, `rcs` and
/// `imessagelite`; the empty string is [`ServiceClass::Empty`] and anything
/// else is [`ServiceClass::Other`]. The raw value is kept in the record.
/// `IMessageLite` is a literal match of the string `iMessageLite`; no
/// meaning (such as satellite messaging) is claimed for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ServiceClass {
    /// `iMessage`.
    IMessage,
    /// `SMS`.
    Sms,
    /// `RCS`.
    Rcs,
    /// `iMessageLite`.
    IMessageLite,
    /// The empty string.
    Empty,
    /// Any other value; counted, never echoed.
    Other,
}

impl ServiceClass {
    /// Classify raw bytes from the database.
    pub fn classify(raw: &[u8]) -> Self {
        if raw.is_empty() {
            ServiceClass::Empty
        } else if raw.eq_ignore_ascii_case(b"imessage") {
            ServiceClass::IMessage
        } else if raw.eq_ignore_ascii_case(b"sms") {
            ServiceClass::Sms
        } else if raw.eq_ignore_ascii_case(b"rcs") {
            ServiceClass::Rcs
        } else if raw.eq_ignore_ascii_case(b"imessagelite") {
            ServiceClass::IMessageLite
        } else {
            ServiceClass::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_classes() {
        for (raw, want) in [
            (&b"iMessage"[..], ServiceClass::IMessage),
            (b"IMESSAGE", ServiceClass::IMessage),
            (b"imessage", ServiceClass::IMessage),
            (b"SMS", ServiceClass::Sms),
            (b"sms", ServiceClass::Sms),
            (b"RCS", ServiceClass::Rcs),
            (b"iMessageLite", ServiceClass::IMessageLite),
            (b"", ServiceClass::Empty),
            (b" SMS", ServiceClass::Other),
            (b"SMS ", ServiceClass::Other),
            (b"iMessage\0", ServiceClass::Other),
            (b"satellite", ServiceClass::Other),
            (&[0xff, 0xfe][..], ServiceClass::Other),
            // Non-ASCII case folding is not applied: U+0130 is not "I".
            ("\u{130}message".as_bytes(), ServiceClass::Other),
        ] {
            assert_eq!(ServiceClass::classify(raw), want, "{raw:?}");
        }
    }
}
