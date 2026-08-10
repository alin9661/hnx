//! Stable domain models shared by the network, cache, and presentation layers.

use std::{collections::HashSet, fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A canonical Hacker News feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Feed {
    Top,
    New,
    Best,
    Ask,
    Show,
    Jobs,
}

impl Feed {
    pub const ALL: [Self; 6] = [
        Self::Top,
        Self::New,
        Self::Best,
        Self::Ask,
        Self::Show,
        Self::Jobs,
    ];

    /// The stable, command-line-friendly feed name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::New => "new",
            Self::Best => "best",
            Self::Ask => "ask",
            Self::Show => "show",
            Self::Jobs => "jobs",
        }
    }

    /// A short human-readable label suitable for terminal navigation.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Top => "Top",
            Self::New => "New",
            Self::Best => "Best",
            Self::Ask => "Ask HN",
            Self::Show => "Show HN",
            Self::Jobs => "Jobs",
        }
    }

    /// The Firebase REST path relative to the API's `/v0/` base URL.
    #[must_use]
    pub const fn firebase_path(self) -> &'static str {
        match self {
            Self::Top => "topstories.json",
            Self::New => "newstories.json",
            Self::Best => "beststories.json",
            Self::Ask => "askstories.json",
            Self::Show => "showstories.json",
            Self::Jobs => "jobstories.json",
        }
    }
}

impl fmt::Display for Feed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Feed {
    type Err = ParseFeedError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "top" | "topstories" => Ok(Self::Top),
            "new" | "newest" | "newstories" => Ok(Self::New),
            "best" | "beststories" => Ok(Self::Best),
            "ask" | "askhn" | "ask-hn" | "askstories" => Ok(Self::Ask),
            "show" | "showhn" | "show-hn" | "showstories" => Ok(Self::Show),
            "job" | "jobs" | "jobstories" => Ok(Self::Jobs),
            _ => Err(ParseFeedError(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown Hacker News feed `{0}`")]
pub struct ParseFeedError(String);

/// The service that supplied a network response (or a cached copy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Algolia,
    Firebase,
    Hybrid,
    Cache,
}

impl Source {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Algolia => "algolia",
            Self::Firebase => "firebase",
            Self::Hybrid => "hybrid",
            Self::Cache => "cache",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Algolia => "Algolia",
            Self::Firebase => "Firebase",
            Self::Hybrid => "Algolia + Firebase",
            Self::Cache => "Cache",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A Hacker News item, including stories, jobs, polls, and comment search hits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub time: i64,
    #[serde(default)]
    pub score: i64,
    #[serde(default)]
    pub descendants: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kids: Vec<u64>,
    /// Poll option ids from Firebase's `parts` field.
    #[serde(default, skip_serializing_if = "slice_is_empty")]
    pub parts: Box<[u64]>,
    /// Poll option records embedded by Algolia.
    #[serde(default, skip_serializing_if = "slice_is_empty")]
    pub poll_options: Box<[PollOption]>,
    /// The parent poll for `pollopt` items, or parent story for comment search hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub dead: bool,
    #[serde(default, alias = "type")]
    pub item_type: String,
}

/// A voting option belonging to a Hacker News poll.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollOption {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub time: i64,
    #[serde(default)]
    pub score: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub dead: bool,
}

impl Item {
    #[must_use]
    pub fn display_title(&self) -> &str {
        self.title
            .as_deref()
            .or(self.text.as_deref())
            .unwrap_or("[untitled]")
    }

    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        self.deleted || self.dead
    }

    #[must_use]
    pub fn is_job(&self) -> bool {
        self.item_type.eq_ignore_ascii_case("job")
    }
}

/// A comment node. `children` retains the server-provided sibling order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub time: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kids: Vec<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Self>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub dead: bool,
    #[serde(default)]
    pub depth: u32,
}

impl Comment {
    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        self.deleted || self.dead
    }

    /// The number of this node and all of its descendants.
    #[must_use]
    pub fn subtree_len(&self) -> usize {
        let mut count = 0_usize;
        let mut pending = vec![self];
        while let Some(comment) = pending.pop() {
            count = count.saturating_add(1);
            pending.extend(comment.children.iter());
        }
        count
    }
}

/// A page of stories, or search hits when `query` is populated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryPage {
    pub feed: Feed,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default)]
    pub items: Vec<Item>,
    pub source: Source,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub fetched_at: i64,
}

impl StoryPage {
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// A story and its recursive comment tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub item: Item,
    #[serde(default)]
    pub comments: Vec<Comment>,
    pub source: Source,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub fetched_at: i64,
}

impl Thread {
    #[must_use]
    pub fn comment_count(&self) -> usize {
        let mut count = 0_usize;
        let mut pending: Vec<_> = self.comments.iter().collect();
        while let Some(comment) = pending.pop() {
            count = count.saturating_add(1);
            pending.extend(comment.children.iter());
        }
        count
    }

    /// Derive completeness information from declared `kids` edges and loaded children.
    ///
    /// This remains stable across serialization without adding a required field to the thread
    /// wire format. Unresolved ids cover request failures, cycles, timeouts, and traversal caps.
    #[must_use]
    pub fn metadata(&self) -> ThreadMetadata {
        let loaded_comments = self.comment_count();
        let mut unresolved_ids = Vec::new();
        let mut seen_unresolved = HashSet::new();
        collect_unresolved(
            &self.item.kids,
            &self.comments,
            &mut unresolved_ids,
            &mut seen_unresolved,
        );

        let mut pending: Vec<_> = self.comments.iter().collect();
        while let Some(comment) = pending.pop() {
            collect_unresolved(
                &comment.kids,
                &comment.children,
                &mut unresolved_ids,
                &mut seen_unresolved,
            );
            pending.extend(comment.children.iter());
        }

        let declared_comments = self.item.descendants;
        let loaded_as_u64 = u64::try_from(loaded_comments).unwrap_or(u64::MAX);
        let unresolved_as_u64 = u64::try_from(unresolved_ids.len()).unwrap_or(u64::MAX);
        let omitted_comments = declared_comments
            .saturating_sub(loaded_as_u64)
            .max(unresolved_as_u64);

        ThreadMetadata {
            partial: omitted_comments > 0 || !unresolved_ids.is_empty(),
            loaded_comments,
            declared_comments,
            omitted_comments,
            unresolved_ids,
        }
    }

    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.metadata().partial
    }
}

/// Completeness information for a fetched comment tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadMetadata {
    pub partial: bool,
    pub loaded_comments: usize,
    pub declared_comments: u64,
    pub omitted_comments: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_ids: Vec<u64>,
}

fn collect_unresolved(
    expected: &[u64],
    loaded: &[Comment],
    unresolved: &mut Vec<u64>,
    seen_unresolved: &mut HashSet<u64>,
) {
    let loaded_ids: HashSet<_> = loaded.iter().map(|comment| comment.id).collect();
    for id in expected {
        if !loaded_ids.contains(id) && seen_unresolved.insert(*id) {
            unresolved.push(*id);
        }
    }
}

const fn slice_is_empty<T>(values: &[T]) -> bool {
    values.is_empty()
}

#[cfg(test)]
mod tests {
    use super::{Comment, Feed, Item, Source, Thread};

    #[test]
    fn feed_round_trips_through_serde_and_display() {
        for feed in Feed::ALL {
            let json = serde_json::to_string(&feed).expect("feed serializes");
            assert_eq!(
                serde_json::from_str::<Feed>(&json).expect("feed deserializes"),
                feed
            );
            assert_eq!(feed.to_string().parse(), Ok(feed));
        }
    }

    #[test]
    fn item_accepts_firebase_type_field() {
        let item: Item =
            serde_json::from_str(r#"{"id":42,"type":"story"}"#).expect("Firebase item parses");

        assert_eq!(item.id, 42);
        assert_eq!(item.item_type, "story");
        assert_eq!(Source::Firebase.label(), "Firebase");
    }

    #[test]
    fn deep_comment_counts_use_an_explicit_stack() {
        let mut root = Comment {
            id: 20_000,
            ..Comment::default()
        };
        for id in (1..20_000).rev() {
            root = Comment {
                id,
                kids: vec![root.id],
                children: vec![root],
                ..Comment::default()
            };
        }
        let mut thread = Thread {
            item: Item {
                id: 42,
                kids: vec![1],
                descendants: 20_000,
                ..Item::default()
            },
            comments: vec![root],
            source: Source::Firebase,
            stale: false,
            fetched_at: 0,
        };

        assert_eq!(thread.comments[0].subtree_len(), 20_000);
        assert_eq!(thread.comment_count(), 20_000);
        assert!(!thread.is_partial());

        // Dismantle iteratively as well so the test itself does not exercise recursive drop.
        let mut current = thread.comments.pop();
        while let Some(mut comment) = current {
            current = comment.children.pop();
        }
    }
}
