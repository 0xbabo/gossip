use crate::db::DbEvent;
use crate::error::Error;
use nostr_proto::{Event, EventKind, Id};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Reactions {
    pub upvotes: u64,
    pub downvotes: u64,
    pub emojis: Vec<(char, u64)>,
}

/// This contains event-related data that is relevant at the time of
/// rendering the event, most of which is gathered from other related
/// events.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EventRelated {
    pub id: Id,
    pub feed_related: bool,
    pub replies: Vec<Id>,
    pub in_reply_to: Option<Id>,
    pub reactions: Reactions,
    pub deleted_reason: Option<String>,
    pub client: Option<String>,
    pub hashtags: Vec<String>,
    pub subject: Option<String>,
    pub urls: Vec<String>,
    pub last_reply_at: Option<i64>,
}

impl EventRelated {
    #[allow(dead_code)]
    pub fn new(id: Id) -> EventRelated {
        EventRelated {
            id,
            feed_related: false,
            replies: Vec::new(),
            in_reply_to: None,
            reactions: Default::default(),
            deleted_reason: None,
            client: None,
            hashtags: Vec::new(),
            subject: None,
            urls: Vec::new(),
            last_reply_at: None,
        }
    }
}

impl From<&Event> for EventRelated {
    fn from(event: &Event) -> EventRelated {
        EventRelated {
            id: event.id,
            feed_related: event.kind == EventKind::TextNote,
            replies: Vec::new(),
            in_reply_to: None,
            reactions: Default::default(),
            deleted_reason: None,
            client: None,
            hashtags: Vec::new(),
            subject: None,
            urls: Vec::new(),
            last_reply_at: Some(event.created_at.0),
        }
    }
}

impl TryFrom<&DbEvent> for EventRelated {
    type Error = Error;

    fn try_from(dbevent: &DbEvent) -> Result<EventRelated, Error> {
        Ok(EventRelated {
            id: dbevent.id.clone().try_into()?,
            feed_related: dbevent.kind == 1,
            replies: Vec::new(),
            in_reply_to: None,
            reactions: Default::default(),
            deleted_reason: None,
            client: None,
            hashtags: Vec::new(),
            subject: None,
            urls: Vec::new(),
            last_reply_at: Some(dbevent.created_at),
        })
    }
}
