use crate::db::{DbPersonRelay, DbRelay, Direction};
use crate::error::Error;
use crate::globals::GLOBALS;
use dashmap::{DashMap, DashSet};
use nostr_types::{PublicKeyHex, RelayUrl, Unixtime};
use std::fmt;
use tokio::sync::RwLock;

// FIXME: move it into here
use crate::relay_picker::RelayAssignment;

// FIXME: move it into here
use crate::relay_picker::RelayPickerFailure;

/// The RelayPicker2 is a structure that helps assign people we follow to relays we watch.
/// It remembers which publickeys are assigned to which relays, which pubkeys need more
/// relays and how many, which relays need a time out, and person-relay scores for making
/// good assignments dynamically.
#[derive(Debug, Default)]
pub struct RelayPicker2 {
    /// All of the relays we might use
    pub all_relays: DashMap<RelayUrl, DbRelay>,

    /// All of the relays currently connected, with optional assignments.
    /// (Sometimes a relay is connected for a different kind of subscription.)
    pub connected_relays: DashMap<RelayUrl, Option<RelayAssignment>>,

    /// Relays which recently failed and which require a timeout before
    /// they can be chosen again.  The value is the time when it can be removed
    /// from this list.
    pub excluded_relays: DashMap<RelayUrl, i64>,

    /// For each followed pubkey that still needs assignments, the number of relay
    /// assignments it is seeking.  These start out at settings.num_relays_per_person
    /// (or less if the person doesn't have that many relays)
    pub pubkey_counts: DashMap<PublicKeyHex, u8>,

    /// A ranking of relays per person.
    pub person_relay_scores: RwLock<Vec<(PublicKeyHex, RelayUrl, u64)>>,
}

impl RelayPicker2 {
    /// This starts a new RelayPicker that has:
    ///  * All relays
    ///  * All followed public keys, with count starting at num_relays_per_person
    ///  * person relay scores for all person-relay pairings
    pub async fn new() -> Result<RelayPicker2, Error> {
        // Load relays from the database
        let all_relays: DashMap<RelayUrl, DbRelay> = DbRelay::fetch(None).await?.drain(..)
            .map(|dbr| (dbr.url.clone(), dbr)).collect();

        let num_relays_per_person = GLOBALS.settings.read().await.num_relays_per_person;

        let mut person_relay_scores: Vec<(PublicKeyHex, RelayUrl, u64)> = Vec::new();
        let pubkey_counts: DashMap<PublicKeyHex, u8> = DashMap::new();

        // Get all the people we follow
        let pubkeys: Vec<PublicKeyHex> = GLOBALS
            .people
            .get_followed_pubkeys()
            .iter()
            .map(|p| p.to_owned())
            .collect();

        for pubkey in &pubkeys {
            let best_relays: Vec<(PublicKeyHex, RelayUrl, u64)> =
                DbPersonRelay::get_best_relays(pubkey.to_owned(), Direction::Write)
                .await?
                .iter()
                .map(|(url, score)| (pubkey.to_owned(), url.to_owned(), *score))
                .collect();

            let count = num_relays_per_person.min(best_relays.len() as u8);
            pubkey_counts.insert(pubkey.clone(), count);

            person_relay_scores.extend(best_relays);
        }

        Ok(RelayPicker2 {
            all_relays,
            connected_relays: DashMap::new(),
            excluded_relays: DashMap::new(),
            pubkey_counts,
            person_relay_scores: RwLock::new(person_relay_scores),
        })
    }

    pub async fn refresh_person_relay_scores(&mut self) -> Result<(), Error> {
        let mut person_relay_scores: Vec<(PublicKeyHex, RelayUrl, u64)> = Vec::new();

        // Get all the people we follow
        let pubkeys: Vec<PublicKeyHex> = GLOBALS
            .people
            .get_followed_pubkeys()
            .iter()
            .map(|p| p.to_owned())
            .collect();

        // Compute scores for each person_relay pairing
        for pubkey in &pubkeys {
            let best_relays: Vec<(PublicKeyHex, RelayUrl, u64)> =
                DbPersonRelay::get_best_relays(pubkey.to_owned(), Direction::Write)
                .await?
                .iter()
                .map(|(url, score)| (pubkey.to_owned(), url.to_owned(), *score))
                .collect();

            person_relay_scores.extend(best_relays);
        }

        *self.person_relay_scores.write().await = person_relay_scores;

        Ok(())
    }

    /// When a relay disconnects, call this so that whatever assignments it might have
    /// had can be reassigned.  Then call pick_relays() again.
    pub fn relay_disconnected(&mut self, url: &RelayUrl) {

        // Remove from connected relays list
        if let Some((_key, maybe_assignment)) = self.connected_relays.remove(url) {

            // Exclude the relay for the next 30 seconds
            let hence = Unixtime::now().unwrap().0 + 30;
            self.excluded_relays.insert(url.to_owned(), hence);
            tracing::debug!(
                "{} goes into the penalty box until {}",
                url,
                hence,
            );

            // Take any assignment
            if let Some(relay_assignment) = maybe_assignment {

                // Put the public keys back into pubkey_counts
                for pubkey in relay_assignment.pubkeys.iter() {
                    self.pubkey_counts
                        .entry(pubkey.to_owned())
                        .and_modify(|e| *e += 1)
                        .or_insert(1);
                }
            }
        }
    }
}
