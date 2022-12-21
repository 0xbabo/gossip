use crate::comms::BusMessage;
use crate::db::{DbPerson, DbPersonRelay, DbRelay};
use crate::error::Error;
use crate::event_related::EventRelated;
use nostr_proto::{Event, EventKind, Id, Metadata, PublicKey, PublicKeyHex, Tag, Unixtime};
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use tokio::sync::{broadcast, mpsc, Mutex};

/// Only one of these is ever created, via lazy_static!, and represents
/// global state for the rust application
pub struct Globals {
    /// This is our connection to SQLite. Only one thread at a time.
    pub db: Mutex<Option<Connection>>,

    /// This is a broadcast channel. All Minions should listen on it.
    /// To create a receiver, just run .subscribe() on it.
    pub to_minions: broadcast::Sender<BusMessage>,

    /// This is a mpsc channel. The Overlord listens on it.
    /// To create a sender, just clone() it.
    pub to_overlord: mpsc::UnboundedSender<BusMessage>,

    /// This is ephemeral. It is filled during lazy_static initialization,
    /// and stolen away when the Overlord is created.
    pub from_minions: Mutex<Option<mpsc::UnboundedReceiver<BusMessage>>>,

    /// All nostr events currently loaded to memory, keyed by their Id
    pub events: Mutex<HashMap<Id, Event>>,

    /// All nostr event related data, keyed by the event Id
    pub event_relateds: Mutex<HashMap<Id, EventRelated>>,

    /// All nostr people records currently loaded into memory, keyed by pubkey
    pub people: Mutex<HashMap<PublicKey, DbPerson>>,

    /// Whether or not we have a saved private key and need the password to unlock it
    #[allow(dead_code)]
    pub need_password: AtomicBool,
}

lazy_static! {
    pub static ref GLOBALS: Globals = {

        // Setup a communications channel from the Overlord to the Minions.
        let (to_minions, _) = broadcast::channel(16);

        // Setup a communications channel from the Minions to the Overlord.
        let (to_overlord, from_minions) = mpsc::unbounded_channel();

        Globals {
            db: Mutex::new(None),
            to_minions,
            to_overlord,
            from_minions: Mutex::new(Some(from_minions)),
            events: Mutex::new(HashMap::new()),
            event_relateds: Mutex::new(HashMap::new()),
            people: Mutex::new(HashMap::new()),
            need_password: AtomicBool::new(false),
        }
    };
}

#[allow(dead_code)]
pub async fn get_feed() -> Vec<Id> {
    let mut feed: Vec<EventRelated> = GLOBALS
        .event_relateds
        .lock()
        .await
        .iter()
        .map(|(_, e)| e)
        .filter(|e| e.feed_related) // feed related
        .filter(|e| e.in_reply_to.is_none()) // only root events
        .cloned()
        .collect();
    feed.sort_unstable_by(|a, b| a.last_reply_at.cmp(&b.last_reply_at));
    feed.iter().map(|e| e.id).collect()
}

#[allow(dead_code)]
pub fn blocking_get_feed() -> Vec<Id> {
    let mut feed: Vec<EventRelated> = GLOBALS
        .event_relateds
        .blocking_lock()
        .iter()
        .map(|(_, e)| e)
        //.filter(|e| e.feed_related) // feed related
        //.filter(|e| e.in_reply_to.is_none()) // only root events
        .cloned()
        .collect();
    feed.sort_unstable_by(|a, b| a.last_reply_at.cmp(&b.last_reply_at));
    feed.iter().map(|e| e.id).collect()
}

pub async fn add_event(event: &Event) -> Result<(), Error> {
    // Insert the event
    insert_event(event).await;

    // Some kinds seen in the wild:
    //    nonce, p, e, t, client, content-warning,
    //    subject, h, i, nostril, r, hashtag
    for tag in event.tags.iter() {
        // Get some metadata from tags that could apply to multiple
        // kinds of events
        match tag {
            Tag::Event {
                id,
                recommended_relay_url: _,
                marker,
            } => {
                if event.kind == EventKind::TextNote {
                    if let Some(m) = marker {
                        if m == "reply" {
                            // Mark our 'in_reply_to'
                            update_event_related(event.id, |er| {
                                er.in_reply_to = Some(*id);
                            })
                            .await;

                            // Add ourself to the parent's replies
                            update_event_related(*id, |er| {
                                er.replies.push(event.id);
                            })
                            .await;

                            // Update the last_reply_at all the way up the chain
                            let mut xid = *id;
                            loop {
                                let mut in_reply_to: Option<Id> = None;
                                update_event_related(xid, |er| {
                                    if let Some(other) = er.last_reply_at {
                                        er.last_reply_at = Some(other.max(event.created_at.0));
                                    } else {
                                        er.last_reply_at = Some(event.created_at.0);
                                    }
                                    in_reply_to = er.in_reply_to;
                                })
                                .await;

                                xid = match in_reply_to {
                                    Some(ref id) => *id,
                                    None => break,
                                }
                            }
                        }
                    }
                } else if event.kind == EventKind::EventDeletion {
                    // Find the other event
                    if let Some(other_event) = { GLOBALS.events.lock().await.get(id) } {
                        // Make sure the authors match
                        if other_event.pubkey != event.pubkey {
                            // Invalid delete event
                            GLOBALS.events.lock().await.remove(id);
                            GLOBALS.event_relateds.lock().await.remove(id);
                            return Ok(());
                        }
                        update_event_related(*id, |er| {
                            er.deleted_reason = Some(event.content.clone());
                        })
                        .await;
                    } else {
                        // FIXME - currently we don't apply this deletion event
                        // if we don't have the event it refers to because we cannot
                        // check that the authors match.
                        // but if we get the event it refers to later, nothing will
                        // trigger us to reapply it.
                    }
                }
            }
            Tag::Pubkey { .. } => {
                // Maybe we can generally handle these?
                // Maybe it is too specific to certain event types.
                // For now we process these under specific event types.
            }
            Tag::Hashtag(s) => {
                update_event_related(event.id, |er| {
                    er.hashtags.push(s.to_string());
                })
                .await;
            }
            Tag::Reference(r) => {
                update_event_related(event.id, |er| {
                    er.urls.push(r.to_string());
                })
                .await;
            }
            Tag::Geohash(_) => {} // not implemented
            Tag::Subject(s) => {
                update_event_related(event.id, |er| {
                    er.subject = Some(s.to_string());
                })
                .await;
            }
            Tag::Nonce { .. } => {} // not implemented
            Tag::Other { tag, data } => {
                if tag == "client" && !data.is_empty() {
                    update_event_related(event.id, |er| {
                        er.client = Some(data[0].to_string());
                    })
                    .await;
                }
            }
            Tag::Empty => {} // nothing to do
        }
    }

    Ok(())
}

async fn insert_event(event: &Event) {
    let mut events = GLOBALS.events.lock().await;
    events.insert(event.id, event.clone());
}

async fn update_event_related<F>(id: Id, mut f: F)
where
    F: FnMut(&mut EventRelated),
{
    let mut event_relateds = GLOBALS.event_relateds.lock().await;
    let event_related = event_relateds
        .entry(id)
        .or_insert_with(|| EventRelated::new(id));
    f(event_related);
}

#[allow(dead_code)]
async fn update_person_from_event_metadata(
    pubkey: PublicKey,
    created_at: Unixtime,
    metadata: Metadata,
) {
    let mut people = GLOBALS.people.lock().await;
    let person = people
        .entry(pubkey)
        .or_insert_with(|| DbPerson::new(pubkey.into()));

    // Do not update the metadata if ours is newer
    if let Some(metadata_at) = person.metadata_at {
        if created_at.0 <= metadata_at {
            // Old metadata. Ignore it
            return;
        }
    }

    // Update the metadata
    person.name = metadata.name;
    person.about = metadata.about;
    person.picture = metadata.picture;
    if person.dns_id != metadata.nip05 {
        person.dns_id = metadata.nip05;
        person.dns_id_valid = 0; // changed, so reset to invalid
        person.dns_id_last_checked = None; // we haven't checked this one yet
    }
    person.metadata_at = Some(created_at.0);
}

#[allow(dead_code)]
async fn save_person(pubkey: PublicKey) -> Result<(), Error> {
    let mut people = GLOBALS.people.lock().await;
    let person = people
        .entry(pubkey)
        .or_insert_with(|| DbPerson::new(pubkey.into()));

    DbPerson::update(person.clone()).await?;
    Ok(())
}

pub async fn followed_pubkeys() -> Vec<PublicKeyHex> {
    let people = GLOBALS.people.lock().await;
    people
        .iter()
        .map(|(_, p)| p)
        .filter(|p| p.followed == 1)
        .map(|p| p.pubkey.clone())
        .collect()
}

#[allow(dead_code)]
pub async fn follow_key_and_relay(pubkey: String, relay: String) -> Result<DbPerson, String> {
    let pubkeyhex = PublicKeyHex(pubkey.clone());

    // Create or update them
    let person = match DbPerson::fetch_one(pubkeyhex.clone())
        .await
        .map_err(|e| format!("{}", e))?
    {
        Some(mut person) => {
            person.followed = 1;
            DbPerson::update(person.clone())
                .await
                .map_err(|e| format!("{}", e))?;
            person
        }
        None => {
            let mut person = DbPerson::new(pubkeyhex.clone());
            person.followed = 1;
            DbPerson::insert(person.clone())
                .await
                .map_err(|e| format!("{}", e))?;
            person
        }
    };

    // Insert (or ignore) this relay
    DbRelay::insert(DbRelay::new(relay.clone()))
        .await
        .map_err(|e| format!("{}", e))?;

    // Insert (or ignore) this person's relay
    DbPersonRelay::insert(DbPersonRelay {
        person: pubkey,
        relay,
        recommended: 0,
        last_fetched: None,
    })
    .await
    .map_err(|e| format!("{}", e))?;

    // Tell the overlord to update the  minion to watch for their events
    // possibly starting a new minion if necessary.
    // FIXME TODO

    // Reply to javascript with the person which will be set in the store
    Ok(person)
}
