//! Identifiers in exports (M6 spec §4.9, D141, owner question Q5).
//!
//! **Q5 is a lean awaiting the owner's confirmation:** by default every id
//! an export writes is an **export-local ordinal**, never an archive row
//! id, because archive ids have gaps that reveal the archive's size and
//! activity outside the scope. Archive ids and source identifiers are
//! written only under the opt-in "Include archive and source identifiers"
//! ([`IdentifierMode::ArchiveAndSource`], off by default, JSON, CSV and
//! XLSX only), and then every archive id is a decimal string
//! ([`crate::record::Decimal`]).
//!
//! - **Messages:** the 1-based rank of the id in the scope index (the
//!   sorted, distinct in-scope ids). Dense, deterministic, safe integers:
//!   no row is ever skipped for an id above 2^53 − 1.
//! - **Conversations:** the rank of the chat id among the exported
//!   conversations ([`RankOrdinals`]).
//! - **Handles, attachment records, files, sessions and lineages:**
//!   assigned at first reference, in the export's deterministic write
//!   order ([`FirstReference`]).
//!
//! The same [`Ordinals`] serve every format of one export, so a message in
//! two conversation files has one ordinal.

use std::collections::BTreeMap;

use oma_archive::{AttachmentId, BlobId, ChatId, HandleId, MessageId, ScopeIndex};

/// An export-local ordinal, from 1.
pub type Ordinal = u64;

/// Which identifiers an export writes (D141).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum IdentifierMode {
    /// Export-local ordinals only (the default).
    #[default]
    Ordinals,
    /// Ordinals plus archive ids (decimal strings) and source identifiers
    /// (GUIDs, `uncanonicalized_id`, `person_centric_id`, a chat's
    /// `room_name` and `group_id`).
    ArchiveAndSource,
}

/// Ordinals assigned by rank in a fixed, sorted list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RankOrdinals<K: Ord> {
    keys: Vec<K>,
}

impl<K: Ord + Copy> RankOrdinals<K> {
    /// The ranks of `keys` (sorted and deduplicated here).
    pub fn new(mut keys: Vec<K>) -> RankOrdinals<K> {
        keys.sort_unstable();
        keys.dedup();
        RankOrdinals { keys }
    }

    /// `k`'s ordinal, or `None` when it is not in the list.
    pub fn get(&self, k: K) -> Option<Ordinal> {
        let i = self.keys.binary_search(&k).ok()?;
        u64::try_from(i).ok().map(|i| i + 1)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The keys in ordinal order.
    pub fn keys(&self) -> &[K] {
        &self.keys
    }
}

/// Ordinals assigned at first reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstReference<K: Ord> {
    map: BTreeMap<K, Ordinal>,
    order: Vec<K>,
}

impl<K: Ord> Default for FirstReference<K> {
    fn default() -> FirstReference<K> {
        FirstReference {
            map: BTreeMap::new(),
            order: Vec::new(),
        }
    }
}

impl<K: Ord + Copy> FirstReference<K> {
    /// `k`'s ordinal, assigning the next one on its first reference.
    pub fn ordinal(&mut self, k: K) -> Ordinal {
        if let Some(&n) = self.map.get(&k) {
            return n;
        }
        let n = u64::try_from(self.order.len()).unwrap_or(u64::MAX - 1) + 1;
        self.map.insert(k, n);
        self.order.push(k);
        n
    }

    /// `k`'s ordinal when it has been referenced.
    pub fn get(&self, k: K) -> Option<Ordinal> {
        self.map.get(&k).copied()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The keys in ordinal order (the first referenced first).
    pub fn in_order(&self) -> &[K] {
        &self.order
    }
}

/// A message reference resolved against the scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageRef {
    /// In this export, with its ordinal.
    InExport(Ordinal),
    /// Not in this export: written with no id (spec §4.2, C1.5).
    NotInExport,
}

/// Every ordinal of one export.
#[derive(Debug, Clone)]
pub struct Ordinals {
    messages: ScopeIndex,
    conversations: RankOrdinals<ChatId>,
    pub handles: FirstReference<HandleId>,
    pub attachments: FirstReference<AttachmentId>,
    pub files: FirstReference<BlobId>,
    pub sessions: FirstReference<i64>,
    pub lineages: FirstReference<i64>,
    mode: IdentifierMode,
}

impl Ordinals {
    /// The ordinals of an export over `messages` (the scope index) and
    /// `conversations` (the exported conversations).
    pub fn new(messages: ScopeIndex, conversations: Vec<ChatId>, mode: IdentifierMode) -> Ordinals {
        Ordinals {
            messages,
            conversations: RankOrdinals::new(conversations),
            handles: FirstReference::default(),
            attachments: FirstReference::default(),
            files: FirstReference::default(),
            sessions: FirstReference::default(),
            lineages: FirstReference::default(),
            mode,
        }
    }

    pub fn mode(&self) -> IdentifierMode {
        self.mode
    }

    /// The scope index.
    pub fn scope(&self) -> &ScopeIndex {
        &self.messages
    }

    /// A message's ordinal when it is in the scope.
    pub fn message(&self, id: MessageId) -> Option<Ordinal> {
        self.messages.ordinal(id)
    }

    /// A message reference: in this export (with its ordinal) or not.
    pub fn message_ref(&self, id: MessageId) -> MessageRef {
        self.message(id)
            .map_or(MessageRef::NotInExport, MessageRef::InExport)
    }

    /// A conversation's ordinal when it is exported.
    pub fn conversation(&self, id: ChatId) -> Option<Ordinal> {
        self.conversations.get(id)
    }

    /// The exported conversations, in ordinal order.
    pub fn conversations(&self) -> &[ChatId] {
        self.conversations.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_are_dense_and_keep_relative_order_but_not_gaps() {
        let r = RankOrdinals::new(vec![
            ChatId::new(900),
            ChatId::new(3),
            ChatId::new(i64::MAX),
            ChatId::new(3),
        ]);
        assert_eq!(r.len(), 3);
        assert_eq!(r.get(ChatId::new(3)), Some(1));
        assert_eq!(r.get(ChatId::new(900)), Some(2));
        // An id above 2^53 - 1 is never skipped: it gets the next rank.
        assert_eq!(r.get(ChatId::new(i64::MAX)), Some(3));
        assert_eq!(r.get(ChatId::new(4)), None);
    }

    #[test]
    fn first_reference_is_deterministic() {
        let mut f = FirstReference::default();
        assert_eq!(f.ordinal(HandleId::new(50)), 1);
        assert_eq!(f.ordinal(HandleId::new(7)), 2);
        assert_eq!(f.ordinal(HandleId::new(50)), 1);
        assert_eq!(f.get(HandleId::new(8)), None);
        assert_eq!(f.in_order(), &[HandleId::new(50), HandleId::new(7)]);
    }

    #[test]
    fn archive_ids_are_decimal_strings() {
        use crate::record::Decimal;
        assert_eq!(Decimal::of(1 << 62).as_str(), "4611686018427387904");
        assert_eq!(Decimal::of(-5).as_str(), "-5");
        assert_eq!(
            serde_json::to_string(&Decimal::of(i64::MAX)).expect("json"),
            "\"9223372036854775807\""
        );
        assert_eq!(format!("{:?}", Decimal::of(12345)), "Decimal(5 chars)");
        assert_eq!(IdentifierMode::default(), IdentifierMode::Ordinals);
    }
}
