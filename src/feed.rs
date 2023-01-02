use crate::globals::GLOBALS;
use nostr_types::{Event, EventKind, Id};
use std::time::{Duration, Instant};

pub struct Feed {
    feed: Vec<Id>,

    // We only recompute the feed at specified intervals
    interval_ms: u32,
    last_computed: Instant,

    // We track these to update subscriptions on them
    my_event_ids: Vec<Id>,
    followed_event_ids: Vec<Id>,
}

impl Feed {
    pub fn new() -> Feed {
        Feed {
            feed: Vec::new(),
            interval_ms: 1000, // Every second, until we load from settings
            last_computed: Instant::now(),
            my_event_ids: Vec::new(),
            followed_event_ids: Vec::new(),
        }
    }

    pub fn get(&mut self) -> Vec<Id> {
        let now = Instant::now();
        if self.last_computed + Duration::from_millis(self.interval_ms as u64) < now {
            self.recompute();
            self.last_computed = now;
        }

        self.feed.clone()
    }

    #[allow(dead_code)]
    pub fn get_my_event_ids(&self) -> Vec<Id> {
        // we assume the main get() happens fast enough to recompute for us.
        self.my_event_ids.clone()
    }

    #[allow(dead_code)]
    pub fn get_followed_event_ids(&self) -> Vec<Id> {
        // we assume the main get() happens fast enough to recompute for us.
        self.followed_event_ids.clone()
    }

    fn recompute(&mut self) {
        let settings = GLOBALS.settings.blocking_read().clone();
        self.interval_ms = settings.feed_recompute_interval_ms;

        let events: Vec<Event> = GLOBALS
            .events
            .blocking_read()
            .iter()
            .map(|(_, e)| e)
            .filter(|e| e.kind == EventKind::TextNote)
            .map(|e| e.to_owned())
            .collect();

        // My event ids
        if let Some(pubkey) = GLOBALS.signer.blocking_read().public_key() {
            self.my_event_ids = events
                .iter()
                .filter_map(|e| if e.pubkey == pubkey { Some(e.id) } else { None })
                .collect();
        } else {
            self.my_event_ids = vec![];
        }

        // Followed event ids
        let followed_pubkeys = GLOBALS.people.blocking_read().get_followed_pubkeys();
        self.followed_event_ids = events
            .iter()
            .filter_map(|e| {
                if followed_pubkeys.contains(&e.pubkey.into()) {
                    Some(e.id)
                } else {
                    None
                }
            })
            .collect();

        // Filter further for the feed
        let mut events: Vec<Event> = events
            .iter()
            .filter(|e| !GLOBALS.dismissed.blocking_read().contains(&e.id))
            .filter(|e| {
                if settings.view_threaded {
                    e.replies_to().is_none()
                } else {
                    true
                }
            })
            .cloned()
            .collect();

        if settings.view_threaded {
            events.sort_unstable_by(|a, b| {
                let a_last = GLOBALS.last_reply.blocking_read().get(&a.id).cloned();
                let b_last = GLOBALS.last_reply.blocking_read().get(&b.id).cloned();
                let a_time = a_last.unwrap_or(a.created_at);
                let b_time = b_last.unwrap_or(b.created_at);
                b_time.cmp(&a_time)
            });
        } else {
            events.sort_unstable_by(|a, b| b.created_at.cmp(&a.created_at));
        }

        self.feed = events.iter().map(|e| e.id).collect();
    }
}
