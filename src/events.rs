use crate::error::Error;
use crate::globals::GLOBALS;
use async_recursion::async_recursion;
use dashmap::{DashMap, DashSet};
use nostr_types::{Event, Id};
use tokio::task;

pub struct Events {
    events: DashMap<Id, Event>,
    new_events: DashSet<Id>,
}

impl Events {
    pub fn new() -> Events {
        Events {
            events: DashMap::new(),
            new_events: DashSet::new(),
        }
    }

    pub fn insert(&self, event: Event) {
        let _ = self.new_events.insert(event.id);
        let _ = self.events.insert(event.id, event);
    }

    #[allow(dead_code)]
    pub fn contains_key(&self, id: &Id) -> bool {
        self.events.contains_key(id)
    }

    pub fn get(&self, id: &Id) -> Option<Event> {
        self.events.get(id).map(|e| e.value().to_owned())
    }

    /// Get the event from memory, and also try the database
    #[allow(dead_code)]
    pub async fn get_local(&self, id: Id) -> Result<Option<Event>, Error> {
        if let Some(e) = self.get(&id) {
            return Ok(Some(e));
        }

        if let Some(event) = task::spawn_blocking(move || {
            let maybe_db = GLOBALS.db.blocking_lock();
            let db = maybe_db.as_ref().unwrap();
            let mut stmt = db.prepare("SELECT raw FROM event WHERE id=?")?;
            stmt.raw_bind_parameter(1, id.as_hex_string())?;
            let mut rows = stmt.raw_query();
            if let Some(row) = rows.next()? {
                let s: String = row.get(0)?;
                Ok(Some(serde_json::from_str(&s)?))
            } else {
                Ok::<Option<Event>, Error>(None)
            }
        })
        .await??
        {
            // Process that event
            crate::process::process_new_event(&event, false, None, None).await?;

            self.insert(event.clone());
            Ok(Some(event))
        } else {
            Ok(None)
        }
    }

    #[allow(dead_code)]
    #[async_recursion]
    pub async fn get_highest_local_parent(&self, id: &Id) -> Result<Option<Id>, Error> {
        if let Some(event) = self.get_local(*id).await? {
            if let Some((parent_id, _opturl)) = event.replies_to() {
                match self.get_highest_local_parent(&parent_id).await? {
                    Some(top_id) => Ok(Some(top_id)), // went higher
                    None => Ok(Some(*id)),            // couldn't go higher, stay here
                }
            } else {
                Ok(Some(*id)) // is a root
            }
        } else {
            Ok(None) // not present locally
        }
    }

    pub fn is_new(&self, id: &Id) -> bool {
        self.new_events.contains(id)
    }

    pub fn clear_new(&self) {
        self.new_events.clear();
    }

    pub fn iter(&self) -> dashmap::iter::Iter<Id, Event> {
        self.events.iter()
    }
}
