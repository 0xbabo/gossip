use crate::comms::{ToMinionMessage, ToOverlordMessage};
use crate::db::DbRelay;
use crate::events::Events;
use crate::feed::Feed;
use crate::fetcher::Fetcher;
use crate::people::People;
use crate::relationship::Relationship;
use crate::relay_picker::RelayPicker;
use crate::settings::Settings;
use crate::signer::Signer;
use nostr_types::{Event, Id, Profile, PublicKeyHex, RelayUrl};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

/// Only one of these is ever created, via lazy_static!, and represents
/// global state for the rust application
pub struct Globals {
    /// This is our connection to SQLite. Only one thread at a time.
    pub db: Mutex<Option<Connection>>,

    /// This is a broadcast channel. All Minions should listen on it.
    /// To create a receiver, just run .subscribe() on it.
    pub to_minions: broadcast::Sender<ToMinionMessage>,

    /// This is a mpsc channel. The Overlord listens on it.
    /// To create a sender, just clone() it.
    pub to_overlord: mpsc::UnboundedSender<ToOverlordMessage>,

    /// This is ephemeral. It is filled during lazy_static initialization,
    /// and stolen away when the Overlord is created.
    pub tmp_overlord_receiver: Mutex<Option<mpsc::UnboundedReceiver<ToOverlordMessage>>>,

    /// All nostr events currently in memory, keyed by the event Id, as well as
    /// information about if they are new or not, and functions
    pub events: Events,

    /// Events coming in from relays that are not processed yet
    /// stored with Url they came from and Subscription they came in on
    pub incoming_events: RwLock<Vec<(Event, RelayUrl, Option<String>)>>,

    /// All relationships between events
    pub relationships: RwLock<HashMap<Id, Vec<(Id, Relationship)>>>,

    /// All nostr people records currently loaded into memory, keyed by pubkey
    pub people: People,

    /// The relay picker, used to pick the next relay
    pub relay_picker: RelayPicker,

    /// Whether or not we are shutting down. For the UI (minions will be signaled and
    /// waited for by the overlord)
    pub shutting_down: AtomicBool,

    /// Settings
    pub settings: RwLock<Settings>,

    /// Signer
    pub signer: Signer,

    /// Dismissed Events
    pub dismissed: RwLock<Vec<Id>>,

    /// Feed
    pub feed: Feed,

    /// Fetcher
    pub fetcher: Fetcher,

    /// Failed Avatar Fetches
    pub failed_avatars: RwLock<HashSet<PublicKeyHex>>,

    pub pixels_per_point_times_100: AtomicU32,

    /// UI status message
    pub status_message: RwLock<String>,

    pub pull_following_merge: AtomicBool,
}

lazy_static! {
    pub static ref GLOBALS: Globals = {

        // Setup a communications channel from the Overlord to the Minions.
        let (to_minions, _) = broadcast::channel(256);

        // Setup a communications channel from the Minions to the Overlord.
        let (to_overlord, tmp_overlord_receiver) = mpsc::unbounded_channel();

        Globals {
            db: Mutex::new(None),
            to_minions,
            to_overlord,
            tmp_overlord_receiver: Mutex::new(Some(tmp_overlord_receiver)),
            events: Events::new(),
            incoming_events: RwLock::new(Vec::new()),
            relationships: RwLock::new(HashMap::new()),
            people: People::new(),
            relay_picker: Default::default(),
            shutting_down: AtomicBool::new(false),
            settings: RwLock::new(Settings::default()),
            signer: Signer::default(),
            dismissed: RwLock::new(Vec::new()),
            feed: Feed::new(),
            fetcher: Fetcher::new(),
            failed_avatars: RwLock::new(HashSet::new()),
            pixels_per_point_times_100: AtomicU32::new(139), // 100 dpi, 1/72th inch => 1.38888
            status_message: RwLock::new("Welcome to Gossip. Status messages will appear here. Click them to dismiss them.".to_owned()),
            pull_following_merge: AtomicBool::new(true),
        }
    };
}

impl Globals {
    /*
    pub async fn get_local_event(id: Id) -> Option<Event> {
        // Try memory
        if let Some(e) = GLOBALS.events.get(&id) {
            return Some(e.to_owned())
        }

        // Try the database

    }
     */

    pub async fn add_relationship(id: Id, related: Id, relationship: Relationship) {
        let r = (related, relationship);
        let mut relationships = GLOBALS.relationships.write().await;
        relationships
            .entry(id)
            .and_modify(|vec| {
                if !vec.contains(&r) {
                    vec.push(r.clone());
                }
            })
            .or_insert_with(|| vec![r]);
    }

    pub fn get_replies_sync(id: Id) -> Vec<Id> {
        let mut output: Vec<Id> = Vec::new();
        if let Some(vec) = GLOBALS.relationships.blocking_read().get(&id) {
            for (id, relationship) in vec.iter() {
                if *relationship == Relationship::Reply {
                    output.push(*id);
                }
            }
        }

        output
    }

    // FIXME - this allows people to react many times to the same event, and
    //         it counts them all!
    pub fn get_reactions_sync(id: Id) -> Vec<(char, usize)> {
        let mut output: HashMap<char, usize> = HashMap::new();

        if let Some(relationships) = GLOBALS.relationships.blocking_read().get(&id) {
            for (_id, relationship) in relationships.iter() {
                if let Relationship::Reaction(reaction) = relationship {
                    if let Some(ch) = reaction.chars().next() {
                        output
                            .entry(ch)
                            .and_modify(|count| *count += 1)
                            .or_insert_with(|| 1);
                    } else {
                        output
                            .entry('+') // if empty, presumed to be an upvote
                            .and_modify(|count| *count += 1)
                            .or_insert_with(|| 1);
                    }
                }
            }
        }

        let mut v: Vec<(char, usize)> = output.iter().map(|(c, u)| (*c, *u)).collect();
        v.sort();
        v
    }

    pub fn get_deletion_sync(id: Id) -> Option<String> {
        if let Some(relationships) = GLOBALS.relationships.blocking_read().get(&id) {
            for (_id, relationship) in relationships.iter() {
                if let Relationship::Deletion(deletion) = relationship {
                    return Some(deletion.clone());
                }
            }
        }
        None
    }

    pub fn get_your_nprofile() -> Option<Profile> {
        let public_key = match GLOBALS.signer.public_key() {
            Some(pk) => pk,
            None => return None,
        };

        let mut profile = Profile {
            pubkey: public_key,
            relays: Vec::new(),
        };

        for ri in GLOBALS.relay_picker.all_relays.iter().filter(|ri| ri.value().write) {
            profile.relays.push(ri.key().to_unchecked_url())
        }

        Some(profile)
    }

    pub fn relays_filtered<F>(&self, mut f: F) -> Vec<DbRelay>
    where
        F: FnMut(&DbRelay) -> bool,
    {
        self.relay_picker
            .all_relays
            .iter()
            .filter_map(|r| {
                if f(&r.value()) {
                    Some(r.value().clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn relays_url_filtered<F>(&self, mut f: F) -> Vec<RelayUrl>
    where
        F: FnMut(&DbRelay) -> bool,
    {
        self.relay_picker
            .all_relays
            .iter()
            .filter_map(|r| {
                if f(&r.value()) {
                    Some(r.key().clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn relay_is_connected(&self, url: &RelayUrl) -> bool {
        self.relay_picker.connected_relays.contains(url)
    }
}
