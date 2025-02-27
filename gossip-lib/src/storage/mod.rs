const MAX_LMDB_KEY: usize = 511;

macro_rules! key {
    ($slice:expr) => {
        if $slice.len() > 511 {
            &$slice[..=510]
        } else {
            $slice
        }
    };
}

mod migrations;

// type implementations
pub mod types;

// database implementations
mod event_ek_c_index1;
mod event_ek_pk_index1;
mod event_seen_on_relay1;
mod event_tag_index1;
mod event_viewed1;
mod events1;
mod events2;
mod events3;
mod hashtags1;
mod nip46servers1;
mod nip46servers2;
mod people1;
mod people2;
mod person_lists1;
mod person_lists2;
mod person_lists_metadata1;
mod person_lists_metadata2;
mod person_lists_metadata3;
mod person_relays1;
mod relationships1;
mod relationships_by_addr1;
mod relationships_by_id1;
mod relays1;
mod relays2;
mod reprel1;
mod unindexed_giftwraps1;
mod versioned;

use crate::dm_channel::{DmChannel, DmChannelData};
use crate::error::{Error, ErrorKind};
use crate::globals::GLOBALS;
use crate::nip46::{Nip46Server, Nip46UnconnectedServer};
use crate::people::{Person, PersonList, PersonListMetadata};
use crate::person_relay::PersonRelay;
use crate::profile::Profile;
use crate::relationship::{RelationshipByAddr, RelationshipById};
use crate::relay::Relay;
use gossip_relay_picker::Direction;
use heed::types::UnalignedSlice;
use heed::{Database, Env, EnvFlags, EnvOpenOptions, RwTxn};
use nostr_types::{
    EncryptedPrivateKey, Event, EventAddr, EventKind, EventReference, Id, MilliSatoshi, PublicKey,
    RelayUrl, Unixtime,
};
use paste::paste;
use speedy::{Readable, Writable};
use std::collections::{HashMap, HashSet};
use std::ops::Bound;

use self::event_tag_index1::INDEXED_TAGS;

// Macro to define read-and-write into "general" database, largely for settings
// The type must implemented Speedy Readable and Writable
macro_rules! def_setting {
    ($field:ident, $string:literal, $type:ty, $default:expr) => {
        paste! {
            #[allow(dead_code)]
            pub fn [<write_setting_ $field>]<'a>(
                &'a self,
                $field: &$type,
                rw_txn: Option<&mut RwTxn<'a>>,
            ) -> Result<(), Error> {
                let bytes = $field.write_to_vec()?;

                let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
                    Ok(self.general.put(txn, $string, &bytes)?)
                };

                match rw_txn {
                    Some(txn) => {
                        f(txn)?;
                    }
                    None => {
                        let mut txn = self.env.write_txn()?;
                        f(&mut txn)?;
                        txn.commit()?;
                    }
                };

                Ok(())
            }

            #[allow(dead_code)]
            pub fn [<read_setting_ $field>](&self) -> $type {
                let txn = match self.env.read_txn() {
                    Ok(txn) => txn,
                    Err(_) => return $default,
                };

                match self.general.get(&txn, $string) {
                    Err(_) => $default,
                    Ok(None) => $default,
                    Ok(Some(bytes)) => match <$type>::read_from_buffer(bytes) {
                        Ok(val) => val,
                        Err(_) => $default,
                    }
                }
            }

            #[allow(dead_code)]
            pub(crate) fn [<set_default_setting_ $field>]<'a>(
                &'a self,
                rw_txn: Option<&mut RwTxn<'a>>
            ) -> Result<(), Error> {
                self.[<write_setting_ $field>](&$default, rw_txn)
            }

            #[allow(dead_code)]
            pub fn [<get_default_setting_ $field>]() -> $type {
                $default
            }
        }
    };
}

macro_rules! def_flag {
    ($field:ident, $string:literal, $default:expr) => {
        paste! {
            pub fn [<set_flag_ $field>]<'a>(
                &'a self,
                $field: bool,
                rw_txn: Option<&mut RwTxn<'a>>,
            ) -> Result<(), Error> {
                let bytes = $field.write_to_vec()?;

                let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
                    Ok(self.general.put(txn, $string, &bytes)?)
                };

                match rw_txn {
                    Some(txn) => f(txn)?,
                    None => {
                        let mut txn = self.env.write_txn()?;
                        f(&mut txn)?;
                        txn.commit()?;
                    }
                };

                Ok(())
            }

            pub fn [<get_flag_ $field>](&self) -> bool {
                let txn = match self.env.read_txn() {
                    Ok(txn) => txn,
                    Err(_) => return $default,
                };

                match self.general.get(&txn, $string) {
                    Err(_) => $default,
                    Ok(None) => $default,
                    Ok(Some(bytes)) => bool::read_from_buffer(bytes).unwrap_or($default),
                }
            }
        }
    };
}

type RawDatabase = Database<UnalignedSlice<u8>, UnalignedSlice<u8>>;

/// The LMDB storage engine.
///
/// All calls are synchronous but fast so callers can just wait on them.
pub struct Storage {
    env: Env,

    // General database (settings, local_settings)
    general: RawDatabase,
}

impl Storage {
    pub(crate) fn new() -> Result<Storage, Error> {
        let mut builder = EnvOpenOptions::new();
        unsafe {
            builder.flags(EnvFlags::NO_TLS);
        }
        // builder.max_readers(126); // this is the default
        builder.max_dbs(32);

        // This has to be big enough for all the data.
        // Note that it is the size of the map in VIRTUAL address space,
        //   and that it doesn't all have to be paged in at the same time.
        // Some filesystem that doesn't handle sparse files may allocate all
        //   of this, so we don't go too crazy big.
        // NOTE: this cannot be a setting because settings are only available
        //       after the database has been launched.
        builder.map_size(1048576 * 1024 * 24); // 24 GB

        let dir = Profile::current()?.lmdb_dir;
        let env = match builder.open(&dir) {
            Ok(env) => env,
            Err(e) => {
                tracing::error!("Unable to open LMDB at {}", dir.display());
                return Err(e.into());
            }
        };

        let mut txn = env.write_txn()?;

        let general = env
            .database_options()
            .types::<UnalignedSlice<u8>, UnalignedSlice<u8>>()
            .create(&mut txn)?;

        txn.commit()?;

        Ok(Storage { env, general })
    }

    /// Run this after GLOBALS lazy static initialisation, so functions within storage can
    /// access GLOBALS without hanging.
    pub fn init(&self) -> Result<(), Error> {
        // We have to trigger all of the current-version databases into existence
        // because otherwise there will be MVCC visibility problems later having
        // different transactions in parallel
        //
        // old-version databases will be handled by their migration code and only
        // triggered into existence if their migration is necessary.
        let _ = self.db_event_ek_c_index()?;
        let _ = self.db_event_ek_pk_index()?;
        let _ = self.db_event_tag_index()?;
        let _ = self.db_events()?;
        let _ = self.db_event_seen_on_relay()?;
        let _ = self.db_event_viewed()?;
        let _ = self.db_hashtags()?;
        let _ = self.db_nip46servers()?;
        let _ = self.db_people()?;
        let _ = self.db_person_relays()?;
        let _ = self.db_relationships_by_id()?;
        let _ = self.db_relationships_by_addr()?;
        let _ = self.db_relays()?;
        let _ = self.db_unindexed_giftwraps()?;
        let _ = self.db_person_lists()?;
        let _ = self.db_person_lists_metadata()?;

        // Do migrations
        match self.read_migration_level()? {
            Some(level) => self.migrate(level)?,
            None => self.init_from_empty()?,
        }

        Ok(())
    }

    /// Get a write transaction. With it, you can do multiple writes before you commit it.
    /// Bundling multiple writes together is more efficient.
    pub fn get_write_txn(&self) -> Result<RwTxn<'_>, Error> {
        Ok(self.env.write_txn()?)
    }

    /// Sync the data to disk. This happens periodically, but sometimes it's useful to force
    /// it.
    pub fn sync(&self) -> Result<(), Error> {
        self.env.force_sync()?;
        Ok(())
    }

    // Database getters ---------------------------------

    #[inline]
    pub(crate) fn db_event_ek_c_index(&self) -> Result<RawDatabase, Error> {
        self.db_event_ek_c_index1()
    }

    #[inline]
    pub(crate) fn db_event_ek_pk_index(&self) -> Result<RawDatabase, Error> {
        self.db_event_ek_pk_index1()
    }

    #[inline]
    pub(crate) fn db_event_tag_index(&self) -> Result<RawDatabase, Error> {
        self.db_event_tag_index1()
    }

    #[inline]
    pub(crate) fn db_events(&self) -> Result<RawDatabase, Error> {
        self.db_events3()
    }

    #[inline]
    pub(crate) fn db_event_seen_on_relay(&self) -> Result<RawDatabase, Error> {
        self.db_event_seen_on_relay1()
    }

    #[inline]
    pub(crate) fn db_event_viewed(&self) -> Result<RawDatabase, Error> {
        self.db_event_viewed1()
    }

    #[inline]
    pub(crate) fn db_hashtags(&self) -> Result<RawDatabase, Error> {
        self.db_hashtags1()
    }

    #[inline]
    pub(crate) fn db_nip46servers(&self) -> Result<RawDatabase, Error> {
        self.db_nip46servers2()
    }

    #[inline]
    pub(crate) fn db_people(&self) -> Result<RawDatabase, Error> {
        self.db_people2()
    }

    #[inline]
    pub(crate) fn db_person_relays(&self) -> Result<RawDatabase, Error> {
        self.db_person_relays1()
    }

    #[inline]
    pub(crate) fn db_relationships_by_addr(&self) -> Result<RawDatabase, Error> {
        self.db_relationships_by_addr1()
    }

    #[inline]
    pub(crate) fn db_relationships_by_id(&self) -> Result<RawDatabase, Error> {
        self.db_relationships_by_id1()
    }

    #[inline]
    pub(crate) fn db_relays(&self) -> Result<RawDatabase, Error> {
        self.db_relays2()
    }

    #[inline]
    pub(crate) fn db_unindexed_giftwraps(&self) -> Result<RawDatabase, Error> {
        self.db_unindexed_giftwraps1()
    }

    #[inline]
    pub(crate) fn db_person_lists(&self) -> Result<RawDatabase, Error> {
        self.db_person_lists2()
    }

    #[inline]
    pub(crate) fn db_person_lists_metadata(&self) -> Result<RawDatabase, Error> {
        self.db_person_lists_metadata3()
    }

    // Database length functions ---------------------------------

    /// The number of records in the general table
    pub fn get_general_len(&self) -> Result<u64, Error> {
        let txn = self.env.read_txn()?;
        Ok(self.general.len(&txn)?)
    }

    /// The number of records in the event_seen_on table
    #[inline]
    pub fn get_event_seen_on_relay_len(&self) -> Result<u64, Error> {
        self.get_event_seen_on_relay1_len()
    }

    /// The number of records in the event_viewed table
    #[inline]
    pub fn get_event_viewed_len(&self) -> Result<u64, Error> {
        self.get_event_viewed1_len()
    }

    /// The number of records in the hashtags table
    pub fn get_hashtags_len(&self) -> Result<u64, Error> {
        let txn = self.env.read_txn()?;
        Ok(self.db_hashtags()?.len(&txn)?)
    }

    /// The number of records in the nip46servers table
    pub fn get_nip46servers_len(&self) -> Result<u64, Error> {
        let txn = self.env.read_txn()?;
        Ok(self.db_nip46servers()?.len(&txn)?)
    }

    /// The number of records in the relays table
    #[inline]
    pub fn get_relays_len(&self) -> Result<u64, Error> {
        self.get_relays2_len()
    }

    /// The number of records in the event table
    pub fn get_event_len(&self) -> Result<u64, Error> {
        let txn = self.env.read_txn()?;
        Ok(self.db_events()?.len(&txn)?)
    }

    /// The number of records in the event_ek_pk_index table
    pub fn get_event_ek_pk_index_len(&self) -> Result<u64, Error> {
        let txn = self.env.read_txn()?;
        Ok(self.db_event_ek_pk_index()?.len(&txn)?)
    }

    /// The number of records in the event_ek_c_index table
    pub fn get_event_ek_c_index_len(&self) -> Result<u64, Error> {
        let txn = self.env.read_txn()?;
        Ok(self.db_event_ek_c_index()?.len(&txn)?)
    }

    /// The number of records in the event_tag index table
    pub fn get_event_tag_index_len(&self) -> Result<u64, Error> {
        let txn = self.env.read_txn()?;
        Ok(self.db_event_tag_index()?.len(&txn)?)
    }

    /// The number of records in the relationships_by_addr table
    #[inline]
    pub fn get_relationships_by_addr_len(&self) -> Result<u64, Error> {
        let txn = self.env.read_txn()?;
        Ok(self.db_relationships_by_addr()?.len(&txn)?)
    }

    /// The number of records in the relationships_by_id table
    #[inline]
    pub fn get_relationships_by_id_len(&self) -> Result<u64, Error> {
        let txn = self.env.read_txn()?;
        Ok(self.db_relationships_by_id()?.len(&txn)?)
    }

    /// The number of records in the people table
    #[inline]
    pub fn get_people_len(&self) -> Result<u64, Error> {
        self.get_people2_len()
    }

    /// The number of records in the person_relays table
    #[inline]
    pub fn get_person_relays_len(&self) -> Result<u64, Error> {
        self.get_person_relays1_len()
    }

    /// The number of records in the person_lists table
    pub fn get_person_lists_len(&self) -> Result<u64, Error> {
        let txn = self.env.read_txn()?;
        Ok(self.db_person_lists()?.len(&txn)?)
    }

    // Prune -------------------------------------------------------

    /// Remove all events (and related data) with a created_at before `from`
    /// and all related indexes.
    pub fn prune(&self, from: Unixtime) -> Result<usize, Error> {
        // Extract the Ids to delete.
        let txn = self.env.read_txn()?;
        let mut ids: HashSet<Id> = HashSet::new();
        for result in self.db_events()?.iter(&txn)? {
            let (_key, val) = result?;

            if let Some(created_at) = Event::get_created_at_from_speedy_bytes(val) {
                if created_at < from {
                    if let Some(id) = Event::get_id_from_speedy_bytes(val) {
                        ids.insert(id);
                        // Too bad but we can't delete it now, other threads
                        // might try to access it still. We have to delete it from
                        // all the other maps first.
                    }
                }
            }
        }
        drop(txn);

        let mut txn = self.env.write_txn()?;

        // Delete from event_seen_on_relay
        let mut deletions: Vec<Vec<u8>> = Vec::new();
        for id in &ids {
            let start_key: &[u8] = id.as_slice();
            for result in self
                .db_event_seen_on_relay()?
                .prefix_iter(&txn, start_key)?
            {
                let (_key, val) = result?;
                deletions.push(val.to_owned());
            }
        }
        tracing::info!(
            "PRUNE: deleting {} records from event_seen_on_relay",
            deletions.len()
        );
        for deletion in deletions.drain(..) {
            self.db_event_seen_on_relay()?.delete(&mut txn, &deletion)?;
        }

        // Delete from event_viewed
        for id in &ids {
            let _ = self.db_event_viewed()?.delete(&mut txn, id.as_slice());
        }
        tracing::info!("PRUNE: deleted {} records from event_viewed", ids.len());

        // Delete from hashtags
        // (unfortunately since Ids are the values, we have to scan the whole thing)
        let mut deletions: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for result in self.db_hashtags()?.iter(&txn)? {
            let (key, val) = result?;
            let id = Id(val[0..32].try_into()?);
            if ids.contains(&id) {
                deletions.push((key.to_owned(), val.to_owned()));
            }
        }
        tracing::info!("PRUNE: deleting {} records from hashtags", deletions.len());
        for deletion in deletions.drain(..) {
            self.db_hashtags()?
                .delete_one_duplicate(&mut txn, &deletion.0, &deletion.1)?;
        }

        // Delete from relationships
        // (unfortunately because of the 2nd Id in the tag, we have to scan the whole thing)
        let mut deletions: Vec<Vec<u8>> = Vec::new();
        for result in self.db_relationships_by_id()?.iter(&txn)? {
            let (key, _val) = result?;
            let id = Id(key[0..32].try_into()?);
            if ids.contains(&id) {
                deletions.push(key.to_owned());
                continue;
            }
            let id2 = Id(key[32..64].try_into()?);
            if ids.contains(&id2) {
                deletions.push(key.to_owned());
            }
        }
        tracing::info!("PRUNE: deleting {} relationships", deletions.len());
        for deletion in deletions.drain(..) {
            self.db_relationships_by_id()?.delete(&mut txn, &deletion)?;
        }

        // delete from events
        for id in &ids {
            let _ = self.db_events()?.delete(&mut txn, id.as_slice());
        }
        tracing::info!("PRUNE: deleted {} records from events", ids.len());

        txn.commit()?;

        Ok(ids.len())
    }

    // General key-value functions --------------------------------------------------

    pub(crate) fn write_migration_level<'a>(
        &'a self,
        migration_level: u32,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let bytes = migration_level.to_be_bytes();

        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            Ok(self.general.put(txn, b"migration_level", &bytes)?)
        };

        match rw_txn {
            Some(txn) => {
                f(txn)?;
            }
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    pub(crate) fn read_migration_level(&self) -> Result<Option<u32>, Error> {
        let txn = self.env.read_txn()?;

        Ok(self
            .general
            .get(&txn, b"migration_level")?
            .map(|bytes| u32::from_be_bytes(bytes[..4].try_into().unwrap())))
    }

    /// Write the user's encrypted private key
    pub fn write_encrypted_private_key<'a>(
        &'a self,
        epk: Option<&EncryptedPrivateKey>,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let bytes = epk.map(|e| &e.0).write_to_vec()?;

        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            self.general.put(txn, b"encrypted_private_key", &bytes)?;
            Ok(())
        };

        match rw_txn {
            Some(txn) => f(txn)?,
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    /// Read the user's encrypted private key
    pub fn read_encrypted_private_key(&self) -> Result<Option<EncryptedPrivateKey>, Error> {
        let txn = self.env.read_txn()?;

        match self.general.get(&txn, b"encrypted_private_key")? {
            None => Ok(None),
            Some(bytes) => {
                let os = Option::<String>::read_from_buffer(bytes)?;
                Ok(os.map(EncryptedPrivateKey))
            }
        }
    }

    /// Write NIP-46 unconnected server
    #[allow(dead_code)]
    pub fn write_nip46_unconnected_server<'a>(
        &'a self,
        server: &Nip46UnconnectedServer,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let bytes = server.write_to_vec()?;

        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            self.general.put(txn, b"nip46_unconnected_server", &bytes)?;
            Ok(())
        };

        match rw_txn {
            Some(txn) => f(txn)?,
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    /// Read NIP-46 unconnected server
    #[allow(dead_code)]
    pub fn read_nip46_unconnected_server(&self) -> Result<Option<Nip46UnconnectedServer>, Error> {
        let txn = self.env.read_txn()?;
        match self.general.get(&txn, b"nip46_unconnected_server")? {
            None => Ok(None),
            Some(bytes) => {
                let server = Nip46UnconnectedServer::read_from_buffer(bytes)?;
                Ok(Some(server))
            }
        }
    }

    /// Delete a NIP-46 unconnected server
    #[allow(dead_code)]
    pub fn delete_nip46_unconnected_server<'a>(
        &'a self,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            self.general.delete(txn, b"nip46_unconnected_server")?;
            Ok(())
        };

        match rw_txn {
            Some(txn) => f(txn)?,
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    // Flags ------------------------------------------------------------

    def_flag!(following_only, b"following_only", false);
    def_flag!(wizard_complete, b"wizard_complete", false);
    def_flag!(
        rebuild_relationships_needed,
        b"rebuild_relationships_needed",
        false
    );

    // Settings ----------------------------------------------------------

    // This defines functions for read_{setting} and write_{setting} for each
    // setting value
    def_setting!(public_key, b"public_key", Option::<PublicKey>, None);
    def_setting!(log_n, b"log_n", u8, 18);
    def_setting!(login_at_startup, b"login_at_startup", bool, true);
    def_setting!(offline, b"offline", bool, false);
    def_setting!(load_avatars, b"load_avatars", bool, true);
    def_setting!(load_media, b"load_media", bool, true);
    def_setting!(check_nip05, b"check_nip05", bool, true);
    def_setting!(
        automatically_fetch_metadata,
        b"automatically_fetch_metadata",
        bool,
        true
    );
    def_setting!(
        relay_connection_requires_approval,
        b"relay_connection_requires_approval",
        bool,
        false
    );
    def_setting!(
        relay_auth_requires_approval,
        b"relay_auth_requires_approval",
        bool,
        false
    );
    def_setting!(num_relays_per_person, b"num_relays_per_person", u8, 2);
    def_setting!(max_relays, b"max_relays", u8, 50);
    def_setting!(feed_chunk, b"feed_chunk", u64, 60 * 60 * 4);
    def_setting!(replies_chunk, b"replies_chunk", u64, 60 * 60 * 24 * 7);
    def_setting!(
        person_feed_chunk,
        b"person_feed_chunk",
        u64,
        60 * 60 * 24 * 15
    );
    def_setting!(overlap, b"overlap", u64, 300);
    def_setting!(reposts, b"reposts", bool, true);
    def_setting!(show_long_form, b"show_long_form", bool, false);
    def_setting!(show_mentions, b"show_mentions", bool, true);
    def_setting!(direct_messages, b"direct_messages", bool, true);
    def_setting!(
        future_allowance_secs,
        b"future_allowance_secs",
        u64,
        60 * 15
    );
    def_setting!(hide_mutes_entirely, b"hide_mutes_entirely", bool, true);
    def_setting!(reactions, b"reactions", bool, true);
    def_setting!(enable_zap_receipts, b"enable_zap_receipts", bool, true);
    def_setting!(show_media, b"show_media", bool, true);
    def_setting!(
        approve_content_warning,
        b"approve_content_warning",
        bool,
        false
    );
    def_setting!(show_deleted_events, b"show_deleted_events", bool, false);
    def_setting!(pow, b"pow", u8, 0);
    def_setting!(set_client_tag, b"set_client_tag", bool, false);
    def_setting!(set_user_agent, b"set_user_agent", bool, false);
    def_setting!(delegatee_tag, b"delegatee_tag", String, String::new());
    def_setting!(max_fps, b"max_fps", u32, 12);
    def_setting!(
        recompute_feed_periodically,
        b"recompute_feed_periodically",
        bool,
        true
    );
    def_setting!(
        feed_recompute_interval_ms,
        b"feed_recompute_interval_ms",
        u32,
        8000
    );
    def_setting!(
        theme_variant,
        b"theme_variant",
        String,
        "Default".to_owned()
    );
    def_setting!(dark_mode, b"dark_mode", bool, false);
    def_setting!(follow_os_dark_mode, b"follow_os_dark_mode", bool, true);
    def_setting!(override_dpi, b"override_dpi", Option::<u32>, None);
    def_setting!(
        highlight_unread_events,
        b"highlight_unread_events",
        bool,
        true
    );
    def_setting!(posting_area_at_top, b"posting_area_at_top", bool, true);
    def_setting!(status_bar, b"status_bar", bool, false);
    def_setting!(
        image_resize_algorithm,
        b"image_resize_algorithm",
        String,
        "CatmullRom".to_owned()
    );
    def_setting!(inertial_scrolling, b"inertial_scrolling", bool, true);
    def_setting!(mouse_acceleration, b"mouse_acceleration", f32, 1.0);
    def_setting!(
        relay_list_becomes_stale_hours,
        b"relay_list_becomes_stale_hours",
        u64,
        8
    );
    def_setting!(
        metadata_becomes_stale_hours,
        b"metadata_becomes_stale_hours",
        u64,
        8
    );
    def_setting!(
        nip05_becomes_stale_if_valid_hours,
        b"nip05_becomes_stale_if_valid_hours",
        u64,
        8
    );
    def_setting!(
        nip05_becomes_stale_if_invalid_minutes,
        b"nip05_becomes_stale_if_invalid_minutes",
        u64,
        30
    );
    def_setting!(
        avatar_becomes_stale_hours,
        b"avatar_becomes_stale_hours",
        u64,
        8
    );
    def_setting!(
        media_becomes_stale_hours,
        b"media_becomes_stale_hours",
        u64,
        8
    );
    def_setting!(
        max_websocket_message_size_kb,
        b"max_websocket_message_size_kb",
        usize,
        1024
    );
    def_setting!(
        max_websocket_frame_size_kb,
        b"max_websocket_frame_size_kb",
        usize,
        1024
    );
    def_setting!(
        websocket_accept_unmasked_frames,
        b"websocket_accept_unmasked_frames",
        bool,
        false
    );
    def_setting!(
        websocket_connect_timeout_sec,
        b"websocket_connect_timeout_sec",
        u64,
        15
    );
    def_setting!(
        websocket_ping_frequency_sec,
        b"websocket_ping_frequency_sec",
        u64,
        55
    );
    def_setting!(
        fetcher_metadata_looptime_ms,
        b"fetcher_metadata_looptime_ms",
        u64,
        1750
    );
    def_setting!(fetcher_looptime_ms, b"fetcher_looptime_ms", u64, 1800);
    def_setting!(
        fetcher_connect_timeout_sec,
        b"fetcher_connect_timeout_sec",
        u64,
        15
    );
    def_setting!(fetcher_timeout_sec, b"fetcher_timeout_sec", u64, 30);
    def_setting!(
        fetcher_max_requests_per_host,
        b"fetcher_max_requests_per_host",
        usize,
        3
    );
    def_setting!(
        fetcher_host_exclusion_on_low_error_secs,
        b"fetcher_host_exclusion_on_low_error_secs",
        u64,
        30
    );
    def_setting!(
        fetcher_host_exclusion_on_med_error_secs,
        b"fetcher_host_exclusion_on_med_error_secs",
        u64,
        60
    );
    def_setting!(
        fetcher_host_exclusion_on_high_error_secs,
        b"fetcher_host_exclusion_on_high_error_secs",
        u64,
        600
    );
    def_setting!(
        nip11_lines_to_output_on_error,
        b"nip11_lines_to_output_on_error",
        usize,
        10
    );
    def_setting!(prune_period_days, b"prune_period_days", u64, 90);
    def_setting!(cache_prune_period_days, b"cache_prune_period_days", u64, 90);
    def_setting!(
        avoid_spam_on_unsafe_relays,
        b"avoid_spam_on_unsafe_relays",
        bool,
        false
    );

    // -------------------------------------------------------------------

    /// Get personlist metadata
    #[inline]
    pub fn get_person_list_metadata(
        &self,
        list: PersonList,
    ) -> Result<Option<PersonListMetadata>, Error> {
        self.get_person_list_metadata3(list)
    }

    /// Set personlist metadata
    #[inline]
    pub fn set_person_list_metadata<'a>(
        &'a self,
        list: PersonList,
        metadata: &PersonListMetadata,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.set_person_list_metadata3(list, metadata, rw_txn)
    }

    /// Get all person lists with their metadata
    #[inline]
    pub fn get_all_person_list_metadata(
        &self,
    ) -> Result<Vec<(PersonList, PersonListMetadata)>, Error> {
        self.get_all_person_list_metadata3()
    }

    /// Find a person list by "d" tag
    #[inline]
    pub fn find_person_list_by_dtag(
        &self,
        dtag: &str,
    ) -> Result<Option<(PersonList, PersonListMetadata)>, Error> {
        self.find_person_list_by_dtag3(dtag)
    }

    /// Allocate a new person list
    #[inline]
    pub fn allocate_person_list<'a>(
        &'a self,
        metadata: &PersonListMetadata,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<PersonList, Error> {
        self.allocate_person_list3(metadata, rw_txn)
    }

    /// Deallocate an empty person list
    #[inline]
    pub fn deallocate_person_list<'a>(
        &'a self,
        list: PersonList,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.deallocate_person_list3(list, rw_txn)
    }

    pub fn rename_person_list<'a>(
        &'a self,
        list: PersonList,
        newname: String,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let mut md = match self.get_person_list_metadata(list)? {
            Some(md) => md,
            None => return Err(ErrorKind::ListNotFound.into()),
        };
        md.title = newname;
        md.last_edit_time = Unixtime::now().unwrap();
        self.set_person_list_metadata(list, &md, rw_txn)?;
        Ok(())
    }

    /// Add event seen on relay
    #[inline]
    pub fn add_event_seen_on_relay<'a>(
        &'a self,
        id: Id,
        url: &RelayUrl,
        when: Unixtime,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.add_event_seen_on_relay1(id, url, when, rw_txn)
    }

    /// Get event seen on relay
    #[inline]
    pub fn get_event_seen_on_relay(&self, id: Id) -> Result<Vec<(RelayUrl, Unixtime)>, Error> {
        self.get_event_seen_on_relay1(id)
    }

    /// Mark event viewed
    #[inline]
    pub fn mark_event_viewed<'a>(
        &'a self,
        id: Id,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.mark_event_viewed1(id, rw_txn)
    }

    /// Is an event viewed?
    #[inline]
    pub fn is_event_viewed(&self, id: Id) -> Result<bool, Error> {
        self.is_event_viewed1(id)
    }

    /// Associate a hashtag to an event
    #[inline]
    pub fn add_hashtag<'a>(
        &'a self,
        hashtag: &String,
        id: Id,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.add_hashtag1(hashtag, id, rw_txn)
    }

    /// Get events with a given hashtag
    #[inline]
    #[allow(dead_code)]
    pub fn get_event_ids_with_hashtag(&self, hashtag: &String) -> Result<Vec<Id>, Error> {
        self.get_event_ids_with_hashtag1(hashtag)
    }

    /// Write a relay record.
    ///
    /// NOTE: this overwrites. You may wish to read first, or you might prefer
    /// [modify_relay](Storage::modify_relay)
    #[inline]
    pub(crate) fn write_relay<'a>(
        &'a self,
        relay: &Relay,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.write_relay2(relay, rw_txn)
    }

    /// Delete a relay record
    #[inline]
    #[allow(dead_code)]
    pub fn delete_relay<'a>(
        &'a self,
        url: &RelayUrl,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.delete_relay2(url, rw_txn)
    }

    /// Write a new relay record only if it is missing
    pub fn write_relay_if_missing<'a>(
        &'a self,
        url: &RelayUrl,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        if self.read_relay(url)?.is_none() {
            let dbrelay = Relay::new(url.to_owned());
            self.write_relay(&dbrelay, rw_txn)?;
        }
        Ok(())
    }

    /// Modify a relay record
    #[inline]
    pub(crate) fn modify_relay<'a, M>(
        &'a self,
        url: &RelayUrl,
        modify: M,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error>
    where
        M: FnMut(&mut Relay),
    {
        self.modify_relay2(url, modify, rw_txn)
    }

    //// Modify all relay records
    #[inline]
    pub(crate) fn modify_all_relays<'a, M>(
        &'a self,
        modify: M,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error>
    where
        M: FnMut(&mut Relay),
    {
        self.modify_all_relays2(modify, rw_txn)
    }

    /// Read a relay record
    #[inline]
    pub fn read_relay(&self, url: &RelayUrl) -> Result<Option<Relay>, Error> {
        self.read_relay2(url)
    }

    /// Read or create relay
    pub fn read_or_create_relay<'a>(
        &'a self,
        url: &RelayUrl,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<Relay, Error> {
        match self.read_relay(url)? {
            Some(relay) => Ok(relay),
            None => {
                let relay = Relay::new(url.to_owned());
                self.write_relay(&relay, rw_txn)?;
                Ok(relay)
            }
        }
    }

    /// Read matching relay records
    #[inline]
    pub fn filter_relays<F>(&self, f: F) -> Result<Vec<Relay>, Error>
    where
        F: Fn(&Relay) -> bool,
    {
        self.filter_relays2(f)
    }

    /// Process a relay list event
    pub fn process_relay_list(&self, event: &Event) -> Result<(), Error> {
        let mut txn = self.env.write_txn()?;

        // Check if this relay list is newer than the stamp we have for its author
        if let Some(mut person) = self.read_person(&event.pubkey)? {
            // Mark that we received it (changes fetch duration for next time)
            person.relay_list_last_received = Unixtime::now().unwrap().0;

            if let Some(previous_at) = person.relay_list_created_at {
                if event.created_at.0 <= previous_at {
                    return Ok(());
                }
            }

            // Mark when it was created
            person.relay_list_created_at = Some(event.created_at.0);

            // And save those marks in the Person record
            self.write_person(&person, Some(&mut txn))?;
        }

        let mut ours = false;
        if let Some(pubkey) = self.read_setting_public_key() {
            if event.pubkey == pubkey {
                tracing::info!("Processing our own relay list");
                ours = true;

                // Clear all current read/write bits (within the transaction)
                // note: inbox is kind10002 'read', outbox is kind10002 'write'
                self.modify_all_relays(
                    |relay| relay.clear_usage_bits(Relay::INBOX | Relay::OUTBOX),
                    Some(&mut txn),
                )?;
            }
        }

        // Collect the URLs for inbox(read) and outbox(write) specified in the event
        let mut inbox_relays: Vec<RelayUrl> = Vec::new();
        let mut outbox_relays: Vec<RelayUrl> = Vec::new();
        for tag in event.tags.iter() {
            if let Ok((uurl, optmarker)) = tag.parse_relay() {
                if let Ok(relay_url) = RelayUrl::try_from_unchecked_url(&uurl) {
                    if let Some(m) = optmarker {
                        match &*m.trim().to_lowercase() {
                            "read" => {
                                // 'read' means inbox and not outbox
                                inbox_relays.push(relay_url.clone());
                                if ours {
                                    if let Some(mut dbrelay) = self.read_relay(&relay_url)? {
                                        // Update
                                        dbrelay.set_usage_bits(Relay::INBOX);
                                        dbrelay.clear_usage_bits(Relay::OUTBOX);
                                        self.write_relay(&dbrelay, Some(&mut txn))?;
                                    } else {
                                        // Insert missing relay
                                        let mut dbrelay = Relay::new(relay_url.to_owned());
                                        // Since we are creating, we add READ
                                        dbrelay.set_usage_bits(Relay::INBOX | Relay::READ);
                                        self.write_relay(&dbrelay, Some(&mut txn))?;
                                    }
                                }
                            }
                            "write" => {
                                // 'write' means outbox and not inbox
                                outbox_relays.push(relay_url.clone());
                                if ours {
                                    if let Some(mut dbrelay) = self.read_relay(&relay_url)? {
                                        // Update
                                        dbrelay.set_usage_bits(Relay::OUTBOX);
                                        dbrelay.clear_usage_bits(Relay::INBOX);
                                        self.write_relay(&dbrelay, Some(&mut txn))?;
                                    } else {
                                        // Create
                                        let mut dbrelay = Relay::new(relay_url.to_owned());
                                        // Since we are creating, we add WRITE
                                        dbrelay.set_usage_bits(Relay::OUTBOX | Relay::WRITE);
                                        self.write_relay(&dbrelay, Some(&mut txn))?;
                                    }
                                }
                            }
                            _ => {} // ignore unknown marker
                        }
                    } else {
                        // No marker means both inbox and outbox
                        inbox_relays.push(relay_url.clone());
                        outbox_relays.push(relay_url.clone());
                        if ours {
                            if let Some(mut dbrelay) = self.read_relay(&relay_url)? {
                                // Update
                                dbrelay.set_usage_bits(Relay::INBOX | Relay::OUTBOX);
                                self.write_relay(&dbrelay, Some(&mut txn))?;
                            } else {
                                // Create
                                let mut dbrelay = Relay::new(relay_url.to_owned());
                                // Since we are creating, we add READ and WRITE
                                dbrelay.set_usage_bits(
                                    Relay::INBOX | Relay::OUTBOX | Relay::READ | Relay::WRITE,
                                );
                                self.write_relay(&dbrelay, Some(&mut txn))?;
                            }
                        }
                    }
                }
            }
        }

        self.set_relay_list(event.pubkey, inbox_relays, outbox_relays, Some(&mut txn))?;

        txn.commit()?;
        Ok(())
    }

    /// Set the user's relay list
    pub fn set_relay_list<'a>(
        &'a self,
        pubkey: PublicKey,
        read_relays: Vec<RelayUrl>,
        write_relays: Vec<RelayUrl>,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let mut person_relays = self.get_person_relays(pubkey)?;

        'for_read_relays: for relay in &read_relays {
            for pr in &person_relays {
                if pr.url == *relay {
                    continue 'for_read_relays;
                }
            }
            // Not found. Create a new person relay for this
            // (last loop below will set and save)
            let pr = PersonRelay::new(pubkey, relay.clone());
            person_relays.push(pr);
        }

        'for_write_relays: for relay in &write_relays {
            for pr in &person_relays {
                if pr.url == *relay {
                    continue 'for_write_relays;
                }
            }
            // Not found. Create a new person relay for this
            // (last loop below will set and save)
            let pr = PersonRelay::new(pubkey, relay.clone());
            person_relays.push(pr);
        }

        for mut pr in person_relays.drain(..) {
            let orig_read = pr.read;
            let orig_write = pr.write;
            pr.read = read_relays.contains(&pr.url);
            pr.write = write_relays.contains(&pr.url);
            if pr.read != orig_read || pr.write != orig_write {
                // here is some reborrow magic we needed to appease the borrow checker
                if let Some(&mut ref mut v) = rw_txn {
                    self.write_person_relay(&pr, Some(v))?;
                } else {
                    self.write_person_relay(&pr, None)?;
                }
            }
        }

        Ok(())
    }

    /// Write an event
    #[inline]
    pub fn write_event<'a>(
        &'a self,
        event: &Event,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.write_event3(event, rw_txn)
    }

    /// Read an event
    #[inline]
    pub fn read_event(&self, id: Id) -> Result<Option<Event>, Error> {
        self.read_event3(id)
    }

    /// If we have th event
    #[inline]
    pub fn has_event(&self, id: Id) -> Result<bool, Error> {
        self.has_event3(id)
    }

    /// Delete the event
    pub fn delete_event<'a>(&'a self, id: Id, rw_txn: Option<&mut RwTxn<'a>>) -> Result<(), Error> {
        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            // Delete from the events table
            self.delete_event3(id, Some(txn))?;

            // Delete from event_seen_on_relay
            {
                // save the actual keys to delete
                let mut deletions: Vec<Vec<u8>> = Vec::new();

                let start_key: &[u8] = id.as_slice();

                for result in self.db_event_seen_on_relay()?.prefix_iter(txn, start_key)? {
                    let (_key, val) = result?;
                    deletions.push(val.to_owned());
                }

                // actual deletion done in second pass
                // (deleting during interation does not work in LMDB)
                for deletion in deletions.drain(..) {
                    self.db_event_seen_on_relay()?.delete(txn, &deletion)?;
                }
            }

            // Delete from event_viewed
            self.db_event_viewed()?.delete(txn, id.as_slice())?;

            // DO NOT delete from relationships. The related event still applies in case
            // this event comes back, ESPECIALLY deletion relationships!

            // We cannot delete from numerous indexes because the ID
            // is in the value, not in the key.
            //
            // These invalid entries will be deleted next time we
            // rebuild indexes.
            //
            // These include
            //   db_event_hashtags()
            //   db_relationships(), where the ID is the 2nd half of the key
            //   db_reprel()
            //   db_event_ek_pk_index()
            //   db_event_ek_c_index()

            Ok(())
        };

        match rw_txn {
            Some(txn) => f(txn)?,
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    /// Replace any existing event with the passed in event, if it is of a replaceable kind
    /// and is newer.
    pub fn replace_event<'a>(
        &'a self,
        event: &Event,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<bool, Error> {
        if !event.kind.is_replaceable() {
            return Err(ErrorKind::General("Event is not replaceable.".to_owned()).into());
        }

        let existing = self.find_events(
            &[event.kind],
            &[event.pubkey],
            None,
            |e| {
                if event.kind.is_parameterized_replaceable() {
                    e.parameter() == event.parameter()
                } else {
                    true
                }
            },
            false,
        )?;

        let mut found_newer = false;
        for old in existing {
            if old.created_at < event.created_at {
                // here is some reborrow magic we needed to appease the borrow checker
                if let Some(&mut ref mut v) = rw_txn {
                    self.delete_event(old.id, Some(v))?;
                } else {
                    self.delete_event(old.id, None)?;
                }
            } else {
                found_newer = true;
            }
        }

        if found_newer {
            return Ok(false); // this event is not the latest one.
        }

        self.write_event(event, rw_txn)?;

        Ok(true)
    }

    /// Get the matching replaceable event (possibly parameterized)
    /// TBD: optimize this by storing better event indexes
    pub fn get_replaceable_event(
        &self,
        kind: EventKind,
        pubkey: PublicKey,
        parameter: &str,
    ) -> Result<Option<Event>, Error> {
        if !kind.is_replaceable() {
            return Err(ErrorKind::General("Event kind is not replaceable".to_owned()).into());
        }

        Ok(self
            .find_events(
                &[kind],
                &[pubkey],
                None, // any time
                |e| {
                    if kind.is_parameterized_replaceable() {
                        e.parameter().as_deref() == Some(parameter)
                    } else {
                        true
                    }
                },
                true, // sorted in reverse time order
            )?
            .first()
            .cloned())
    }

    /// Find events of given kinds and pubkeys.
    /// You must supply kinds. You can skip the pubkeys and then only kinds will matter.
    fn find_ek_pk_events(
        &self,
        kinds: &[EventKind],
        pubkeys: &[PublicKey],
    ) -> Result<HashSet<Id>, Error> {
        if kinds.is_empty() {
            return Err(ErrorKind::General(
                "find_ek_pk_events() requires some event kinds to be specified.".to_string(),
            )
            .into());
        }

        let mut ids: HashSet<Id> = HashSet::new();
        let txn = self.env.read_txn()?;

        for kind in kinds {
            let ek: u32 = (*kind).into();
            if pubkeys.is_empty() {
                let start_key = ek.to_be_bytes().as_slice().to_owned();
                let iter = self.db_event_ek_pk_index()?.prefix_iter(&txn, &start_key)?;
                for result in iter {
                    let (_key, val) = result?;
                    // Take the event
                    let id = Id(val[0..32].try_into()?);
                    ids.insert(id);
                }
            } else {
                for pubkey in pubkeys {
                    let mut start_key = ek.to_be_bytes().as_slice().to_owned();
                    start_key.extend(pubkey.as_bytes());
                    let iter = self.db_event_ek_pk_index()?.prefix_iter(&txn, &start_key)?;
                    for result in iter {
                        let (_key, val) = result?;
                        // Take the event
                        let id = Id(val[0..32].try_into()?);
                        ids.insert(id);
                    }
                }
            }
        }

        Ok(ids)
    }

    /// Find events of given kinds and after the given time.
    fn find_ek_c_events(&self, kinds: &[EventKind], since: Unixtime) -> Result<HashSet<Id>, Error> {
        if kinds.is_empty() {
            return Err(ErrorKind::General(
                "find_ek_c_events() requires some event kinds to be specified.".to_string(),
            )
            .into());
        }

        let now = Unixtime::now().unwrap();
        let mut ids: HashSet<Id> = HashSet::new();
        let txn = self.env.read_txn()?;

        for kind in kinds {
            let ek: u32 = (*kind).into();
            let mut start_key = ek.to_be_bytes().as_slice().to_owned();
            let mut end_key = start_key.clone();
            start_key.extend((i64::MAX - now.0).to_be_bytes().as_slice()); // work back from now
            end_key.extend((i64::MAX - since.0).to_be_bytes().as_slice()); // until since
            let range = (Bound::Included(&*start_key), Bound::Excluded(&*end_key));
            let iter = self.db_event_ek_c_index()?.range(&txn, &range)?;
            for result in iter {
                let (_key, val) = result?;
                // Take the event
                let id = Id(val[0..32].try_into()?);
                ids.insert(id);
            }
        }

        Ok(ids)
    }

    /// Find events of interest.
    ///
    /// You must specify some event kinds.
    /// If pubkeys is empty, they won't matter.
    /// If since is None, it won't matter.
    ///
    /// The function f is run after the matching-so-far events have been deserialized
    /// to finish filtering, and optionally they are sorted in reverse chronological
    /// order.
    pub fn find_events<F>(
        &self,
        kinds: &[EventKind],
        pubkeys: &[PublicKey],
        since: Option<Unixtime>,
        f: F,
        sort: bool,
    ) -> Result<Vec<Event>, Error>
    where
        F: Fn(&Event) -> bool,
    {
        let ids = self.find_event_ids(kinds, pubkeys, since)?;

        // Now that we have that Ids, fetch the events
        let txn = self.env.read_txn()?;
        let mut events: Vec<Event> = Vec::new();
        for id in ids {
            // this is like self.read_event(), but we supply our existing transaction
            if let Some(bytes) = self.db_events()?.get(&txn, id.as_slice())? {
                let event = Event::read_from_buffer(bytes)?;
                if f(&event) {
                    events.push(event);
                }
            }
        }

        if sort {
            events.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        }

        Ok(events)
    }

    /// Find events of interest. This is just like find_events() but it just gives the Ids,
    /// unsorted.
    ///
    /// You must specify some event kinds.
    /// If pubkeys is empty, they won't matter.
    /// If since is None, it won't matter.
    ///
    /// The function f is run after the matching-so-far events have been deserialized
    /// to finish filtering, and optionally they are sorted in reverse chronological
    /// order.
    pub fn find_event_ids(
        &self,
        kinds: &[EventKind],
        pubkeys: &[PublicKey],
        since: Option<Unixtime>,
    ) -> Result<HashSet<Id>, Error> {
        if kinds.is_empty() {
            return Err(ErrorKind::General(
                "find_events() requires some event kinds to be specified.".to_string(),
            )
            .into());
        }

        // Get the Ids
        let ids = match (pubkeys.is_empty(), since) {
            (true, None) => self.find_ek_pk_events(kinds, pubkeys)?,
            (true, Some(when)) => self.find_ek_c_events(kinds, when)?,
            (false, None) => self.find_ek_pk_events(kinds, pubkeys)?,
            (false, Some(when)) => {
                let group1 = self.find_ek_pk_events(kinds, pubkeys)?;
                let group2 = self.find_ek_c_events(kinds, when)?;
                group1.intersection(&group2).copied().collect()
            }
        };

        Ok(ids)
    }

    /// Search all events for the text, case insensitive. Both content and tags
    /// are searched.
    pub fn search_events(&self, text: &str) -> Result<Vec<Event>, Error> {
        let event_kinds = crate::feed::feed_displayable_event_kinds(true);

        let needle = regex::escape(text.to_lowercase().as_str());
        let re = regex::RegexBuilder::new(needle.as_str())
            .unicode(true)
            .case_insensitive(true)
            .build()?;

        let txn = self.env.read_txn()?;
        let iter = self.db_events()?.iter(&txn)?;
        let mut events: Vec<Event> = Vec::new();
        for result in iter {
            let (_key, val) = result?;

            // event kind must match
            if let Some(kind) = Event::get_kind_from_speedy_bytes(val) {
                if !event_kinds.contains(&kind) {
                    continue;
                }
            } else {
                continue;
            }

            if let Some(content) = Event::get_content_from_speedy_bytes(val) {
                if re.is_match(content.as_ref()) {
                    let event = Event::read_from_buffer(val)?;
                    events.push(event);
                    continue;
                }
            }

            if Event::tag_search_in_speedy_bytes(val, &re)? {
                let event = Event::read_from_buffer(val)?;
                events.push(event);
            }
        }

        events.sort_by(|a, b| {
            // ORDER created_at desc
            b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id))
        });

        Ok(events)
    }

    fn switch_to_rumor<'a>(
        &'a self,
        event: &Event,
        txn: &mut RwTxn<'a>,
    ) -> Result<Option<Event>, Error> {
        self.switch_to_rumor3(event, txn)
    }

    // We don't call this externally. Whenever we write an event, we do this.
    fn write_event_ek_pk_index<'a>(
        &'a self,
        id: Id,
        kind: EventKind,
        pubkey: PublicKey,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            let ek: u32 = kind.into();
            let mut key: Vec<u8> = ek.to_be_bytes().as_slice().to_owned(); // event kind
            key.extend(pubkey.as_bytes()); // pubkey
            let bytes = id.as_slice();

            self.db_event_ek_pk_index()?.put(txn, &key, bytes)?;
            Ok(())
        };

        match rw_txn {
            Some(txn) => f(txn)?,
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    // We don't call this externally. Whenever we write an event, we do this.
    fn write_event_ek_c_index<'a>(
        &'a self,
        id: Id,
        kind: EventKind,
        created_at: Unixtime,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            let ek: u32 = kind.into();
            let mut key: Vec<u8> = ek.to_be_bytes().as_slice().to_owned(); // event kind
            key.extend((i64::MAX - created_at.0).to_be_bytes().as_slice()); // reverse created_at
            let bytes = id.as_slice();

            self.db_event_ek_c_index()?.put(txn, &key, bytes)?;
            Ok(())
        };

        match rw_txn {
            Some(txn) => f(txn)?,
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    // Switch to rumor before calling this.
    fn write_event_tag_index<'a>(
        &'a self,
        event: &Event,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.write_event3_tag_index1(event, rw_txn)
    }

    /// Find events having a given tag, and passing the filter.
    /// Only some tags are indxed: "a", "d", "delegation", and "p" for the gossip user only
    pub fn find_tagged_events<F>(
        &self,
        tagname: &str,
        tagvalue: Option<&str>,
        f: F,
        sort: bool,
    ) -> Result<Vec<Event>, Error>
    where
        F: Fn(&Event) -> bool,
    {
        // Make sure we are asking for something that we have indexed
        if !INDEXED_TAGS.contains(&tagname) {
            return Err(ErrorKind::TagNotIndexed(tagname.to_owned()).into());
        }

        let mut ids: HashSet<Id> = HashSet::new();
        let txn = self.env.read_txn()?;

        let mut start_key: Vec<u8> = tagname.as_bytes().to_owned();
        start_key.push(b'\"'); // double quote separator, unlikely to be inside of a tagname
        if let Some(tv) = tagvalue {
            start_key.extend(tv.as_bytes());
        }
        let start_key = key!(&start_key); // limit the size
        let iter = self.db_event_tag_index()?.prefix_iter(&txn, start_key)?;
        for result in iter {
            let (_key, val) = result?;
            // Take the event
            let id = Id(val[0..32].try_into()?);
            ids.insert(id);
        }

        // Now that we have that Ids, fetch and filter the events
        let txn = self.env.read_txn()?;
        let mut events: Vec<Event> = Vec::new();
        for id in ids {
            // this is like self.read_event(), but we supply our existing transaction
            if let Some(bytes) = self.db_events()?.get(&txn, id.as_slice())? {
                let event = Event::read_from_buffer(bytes)?;
                if f(&event) {
                    events.push(event);
                }
            }
        }

        if sort {
            events.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        }

        Ok(events)
    }

    #[inline]
    pub(crate) fn index_unindexed_giftwraps(&self) -> Result<(), Error> {
        self.index_unindexed_giftwraps1()
    }

    pub(crate) fn get_highest_local_parent_event_id(&self, id: Id) -> Result<Option<Id>, Error> {
        let event = match self.read_event(id)? {
            Some(event) => event,
            None => return Ok(None),
        };

        match event.replies_to() {
            Some(EventReference::Id(parent_id, _opturl, _marker)) => {
                self.get_highest_local_parent_event_id(parent_id)
            }
            Some(EventReference::Addr(ea)) => {
                match self.get_replaceable_event(ea.kind, ea.author, &ea.d)? {
                    Some(event) => self.get_highest_local_parent_event_id(event.id),
                    None => Ok(Some(event.id)),
                }
            }
            None => Ok(Some(event.id)),
        }
    }

    /// Write a relationship between two events
    ///
    /// The second Id relates to the first Id,
    /// e.g. related replies to id, or related deletes id
    #[inline]
    pub(crate) fn write_relationship_by_id<'a>(
        &'a self,
        id: Id,
        related: Id,
        relationship_by_id: RelationshipById,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.write_relationship_by_id1(id, related, relationship_by_id, rw_txn)
    }

    /// Find relationships belonging to the given event
    ///
    /// The found Ids relates to the passed in Id,
    /// e.g. result id replies to id, or result id deletes id
    #[inline]
    pub fn find_relationships_by_id(&self, id: Id) -> Result<Vec<(Id, RelationshipById)>, Error> {
        self.find_relationships_by_id1(id)
    }

    /// Write a relationship between an event and an EventAddr (replaceable)
    #[inline]
    pub(crate) fn write_relationship_by_addr<'a>(
        &'a self,
        addr: EventAddr,
        related: Id,
        relationship_by_addr: RelationshipByAddr,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.write_relationship_by_addr1(addr, related, relationship_by_addr, rw_txn)
    }

    /// Find relationships belonging to the given event to replaceable events
    #[inline]
    pub fn find_relationships_by_addr(
        &self,
        addr: &EventAddr,
    ) -> Result<Vec<(Id, RelationshipByAddr)>, Error> {
        self.find_relationships_by_addr1(addr)
    }

    /// Get replies to the given event
    pub fn get_replies(&self, event: &Event) -> Result<Vec<Id>, Error> {
        let mut output = self.get_non_replaceable_replies(event.id)?;
        output.extend(self.get_replaceable_replies(&EventAddr {
            d: event.parameter().unwrap_or("".to_string()),
            relays: vec![],
            kind: event.kind,
            author: event.pubkey,
        })?);
        Ok(output)
    }

    pub fn get_non_replaceable_replies(&self, id: Id) -> Result<Vec<Id>, Error> {
        Ok(self
            .find_relationships_by_id(id)?
            .iter()
            .filter_map(|(id, rel)| {
                if *rel == RelationshipById::Reply {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect())
    }

    pub fn get_replaceable_replies(&self, addr: &EventAddr) -> Result<Vec<Id>, Error> {
        Ok(self
            .find_relationships_by_addr(addr)?
            .iter()
            .filter_map(|(id, rel)| {
                if *rel == RelationshipByAddr::Reply {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect())
    }

    /// Returns the list of reactions and whether or not this account has already reacted to this event
    pub fn get_reactions(&self, id: Id) -> Result<(Vec<(char, usize)>, bool), Error> {
        // Whether or not the Gossip user already reacted to this event
        let mut self_already_reacted = false;

        // Get the event (once self-reactions get deleted we can remove this)
        let maybe_target_event = self.read_event(id)?;

        // Collect up to one reaction per pubkey
        let mut phase1: HashMap<PublicKey, char> = HashMap::new();
        for (_, rel) in self.find_relationships_by_id(id)? {
            if let RelationshipById::Reaction { by, reaction } = rel {
                if let Some(target_event) = &maybe_target_event {
                    if target_event.pubkey == by {
                        // Do not let people like their own post
                        continue;
                    }
                }
                let symbol: char = if let Some(ch) = reaction.chars().next() {
                    ch
                } else {
                    '+'
                };
                phase1.insert(by, symbol);
                if Some(by) == GLOBALS.identity.public_key() {
                    self_already_reacted = true;
                }
            }
        }

        // Collate by char
        let mut output: HashMap<char, usize> = HashMap::new();
        for (_, symbol) in phase1 {
            output
                .entry(symbol)
                .and_modify(|count| *count += 1)
                .or_insert_with(|| 1);
        }

        let mut v: Vec<(char, usize)> = output.drain().collect();
        v.sort();
        Ok((v, self_already_reacted))
    }

    /// Get the zap total of a given event
    pub fn get_zap_total(&self, id: Id) -> Result<MilliSatoshi, Error> {
        let mut total = MilliSatoshi(0);
        for (_, rel) in self.find_relationships_by_id(id)? {
            if let RelationshipById::ZapReceipt { by: _, amount } = rel {
                total = total + amount;
            }
        }
        Ok(total)
    }

    /// Get whether an event was deleted, and if so the optional reason
    pub fn get_deletions(&self, maybe_deleted_event: &Event) -> Result<Vec<String>, Error> {
        let mut reasons: Vec<String> = Vec::new();

        for (deleting_id, rel) in self.find_relationships_by_id(maybe_deleted_event.id)? {
            if let RelationshipById::Deletion { by, reason } = rel {
                if maybe_deleted_event.delete_author_allowed(by) {
                    // We must have the deletion event to check it
                    if let Some(deleting_event) = self.read_event(deleting_id)? {
                        // Delete must come after event in question
                        if deleting_event.created_at > maybe_deleted_event.created_at {
                            reasons.push(reason);
                        }
                    }
                }
            }
        }

        // Deletes via 'a tags (entire paramterized groups)
        if let Some(parameter) = maybe_deleted_event.parameter() {
            let addr = EventAddr {
                d: parameter,
                relays: vec![],
                kind: maybe_deleted_event.kind,
                author: maybe_deleted_event.pubkey,
            };
            for (deleting_id, rel) in self.find_relationships_by_addr(&addr)? {
                // Must be a deletion relationship
                if let RelationshipByAddr::Deletion { by, reason } = rel {
                    if maybe_deleted_event.delete_author_allowed(by) {
                        // We must have the deletion event to check it
                        if let Some(deleting_event) = self.read_event(deleting_id)? {
                            // Delete must come after event in question
                            if deleting_event.created_at > maybe_deleted_event.created_at {
                                reasons.push(reason);
                            }
                        }
                    }
                }
            }
        }

        Ok(reasons)
    }

    /// Write a person record
    #[inline]
    pub fn write_person<'a>(
        &'a self,
        person: &Person,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.write_person2(person, rw_txn)
    }

    /// Read a person record
    #[inline]
    pub fn read_person(&self, pubkey: &PublicKey) -> Result<Option<Person>, Error> {
        self.read_person2(pubkey)
    }

    /// Write a new person record only if missing
    pub fn write_person_if_missing<'a>(
        &'a self,
        pubkey: &PublicKey,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        if self.read_person(pubkey)?.is_none() {
            let person = Person::new(pubkey.to_owned());
            self.write_person(&person, rw_txn)?;
        }
        Ok(())
    }

    /// Read people matching the filter
    #[inline]
    pub fn filter_people<F>(&self, f: F) -> Result<Vec<Person>, Error>
    where
        F: Fn(&Person) -> bool,
    {
        self.filter_people2(f)
    }

    /// Write a PersonRelay record
    #[inline]
    pub fn write_person_relay<'a>(
        &'a self,
        person_relay: &PersonRelay,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.write_person_relay1(person_relay, rw_txn)
    }

    /// Read a PersonRelay record
    #[inline]
    pub fn read_person_relay(
        &self,
        pubkey: PublicKey,
        url: &RelayUrl,
    ) -> Result<Option<PersonRelay>, Error> {
        self.read_person_relay1(pubkey, url)
    }

    /// get PersonRelay records for a person
    #[inline]
    pub fn get_person_relays(&self, pubkey: PublicKey) -> Result<Vec<PersonRelay>, Error> {
        self.get_person_relays1(pubkey)
    }

    /// Do we have any PersonRelay records for the person?
    #[inline]
    pub fn have_persons_relays(&self, pubkey: PublicKey) -> Result<bool, Error> {
        self.have_persons_relays1(pubkey)
    }

    /// Delete PersonRelay records that match the filter
    #[inline]
    pub fn delete_person_relays<'a, F>(
        &'a self,
        filter: F,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error>
    where
        F: Fn(&PersonRelay) -> bool,
    {
        self.delete_person_relays1(filter, rw_txn)
    }

    /// Get the best relays for a person, given a direction.
    ///
    /// This returns the relays for a person, along with a score, in order of score
    pub fn get_best_relays(
        &self,
        pubkey: PublicKey,
        dir: Direction,
    ) -> Result<Vec<(RelayUrl, u64)>, Error> {
        let person_relays = self.get_person_relays(pubkey)?;

        // Note: the following read_rank and write_rank do not consider our own
        // rank or the success rate.
        let mut ranked_relays = match dir {
            Direction::Write => PersonRelay::write_rank(person_relays),
            Direction::Read => PersonRelay::read_rank(person_relays),
        };

        // Modulate these scores with our local rankings
        for ranked_relay in ranked_relays.iter_mut() {
            let relay = self.read_or_create_relay(&ranked_relay.0, None)?;
            ranked_relay.1 = (ranked_relay.1 as f32
                * (relay.rank as f32 / 3.0)
                * (relay.success_rate() * 2.0)) as u64;
        }

        // Resort
        ranked_relays.sort_by(|(_, score1), (_, score2)| score2.cmp(score1));

        let num_relays_per_person = self.read_setting_num_relays_per_person() as usize;

        // If we can't get enough of them, extend with some of our relays at score=2
        if ranked_relays.len() < (num_relays_per_person + 1) {
            let how_many_more = (num_relays_per_person + 1) - ranked_relays.len();
            let score = 2;
            match dir {
                Direction::Write => {
                    // substitute our read relays
                    let additional: Vec<(RelayUrl, u64)> = self
                        .filter_relays(|r| {
                            // not already in their list
                            !ranked_relays.iter().any(|(url, _)| *url == r.url)
                                && r.has_usage_bits(Relay::READ)
                        })?
                        .iter()
                        .map(|r| (r.url.clone(), score))
                        .take(how_many_more)
                        .collect();

                    ranked_relays.extend(additional);
                }
                Direction::Read => {
                    // substitute our write relays???
                    let additional: Vec<(RelayUrl, u64)> = self
                        .filter_relays(|r| {
                            // not already in their list
                            !ranked_relays.iter().any(|(url, _)| *url == r.url)
                                && r.has_usage_bits(Relay::WRITE)
                        })?
                        .iter()
                        .map(|r| (r.url.clone(), score))
                        .take(how_many_more)
                        .collect();

                    ranked_relays.extend(additional);
                }
            }
        }

        Ok(ranked_relays)
    }

    /// Get all the DM channels with associated data
    pub fn dm_channels(&self) -> Result<Vec<DmChannelData>, Error> {
        let my_pubkey = match GLOBALS.identity.public_key() {
            Some(pk) => pk,
            None => return Ok(Vec::new()),
        };

        let events = self.find_events(
            &[EventKind::EncryptedDirectMessage, EventKind::GiftWrap],
            &[],
            None,
            |event| {
                if event.kind == EventKind::EncryptedDirectMessage {
                    event.pubkey == my_pubkey || event.is_tagged(&my_pubkey)
                    // Make sure if it has tags, only author and my_pubkey
                    // TBD
                } else {
                    event.kind == EventKind::GiftWrap
                }
            },
            false,
        )?;

        // Map from channel to latest-message-time and unread-count
        let mut map: HashMap<DmChannel, DmChannelData> = HashMap::new();

        for event in &events {
            let unread: usize = if event.pubkey == my_pubkey {
                // Do not count self-authored events as unread, irrespective of whether they are viewed
                0
            } else {
                1 - self.is_event_viewed(event.id)? as usize
            };
            if event.kind == EventKind::EncryptedDirectMessage {
                let time = event.created_at;
                let dmchannel = match DmChannel::from_event(event, Some(my_pubkey)) {
                    Some(dmc) => dmc,
                    None => continue,
                };
                if let Some(dmcdata) = map.get_mut(&dmchannel) {
                    if time > dmcdata.latest_message_created_at {
                        dmcdata.latest_message_created_at = time;
                        dmcdata.latest_message_content =
                            GLOBALS.identity.decrypt_event_contents(event).ok();
                    }
                    dmcdata.message_count += 1;
                    dmcdata.unread_message_count += unread;
                } else {
                    map.insert(
                        dmchannel.clone(),
                        DmChannelData {
                            dm_channel: dmchannel,
                            latest_message_created_at: time,
                            latest_message_content: GLOBALS
                                .identity
                                .decrypt_event_contents(event)
                                .ok(),
                            message_count: 1,
                            unread_message_count: unread,
                        },
                    );
                }
            } else if event.kind == EventKind::GiftWrap {
                if let Ok(rumor) = GLOBALS.identity.unwrap_giftwrap(event) {
                    let rumor_event = rumor.into_event_with_bad_signature();
                    let time = rumor_event.created_at;
                    let dmchannel = match DmChannel::from_event(&rumor_event, Some(my_pubkey)) {
                        Some(dmc) => dmc,
                        None => continue,
                    };
                    if let Some(dmcdata) = map.get_mut(&dmchannel) {
                        if time > dmcdata.latest_message_created_at {
                            dmcdata.latest_message_created_at = time;
                            dmcdata.latest_message_content = Some(rumor_event.content.clone());
                        }
                        dmcdata.message_count += 1;
                        dmcdata.unread_message_count += unread;
                    } else {
                        map.insert(
                            dmchannel.clone(),
                            DmChannelData {
                                dm_channel: dmchannel,
                                latest_message_created_at: time,
                                latest_message_content: Some(rumor_event.content.clone()),
                                message_count: 1,
                                unread_message_count: unread,
                            },
                        );
                    }
                }
            }
        }

        let mut output: Vec<DmChannelData> = map.drain().map(|e| e.1).collect();
        output.sort_by(|a, b| {
            b.latest_message_created_at
                .cmp(&a.latest_message_created_at)
                .then(b.unread_message_count.cmp(&a.unread_message_count))
        });
        Ok(output)
    }

    /// Get DM events (by id) in a channel
    pub fn dm_events(&self, channel: &DmChannel) -> Result<Vec<Id>, Error> {
        let my_pubkey = match GLOBALS.identity.public_key() {
            Some(pk) => pk,
            None => return Ok(Vec::new()),
        };

        let mut output: Vec<Event> = self.find_events(
            &[EventKind::EncryptedDirectMessage, EventKind::GiftWrap],
            &[],
            Some(Unixtime(0)),
            |event| {
                if let Some(event_dm_channel) = DmChannel::from_event(event, Some(my_pubkey)) {
                    if event_dm_channel == *channel {
                        return true;
                    }
                }
                false
            },
            false,
        )?;

        // sort
        output.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));

        Ok(output.iter().map(|e| e.id).collect())
    }

    /// Rebuild all the event indices. This is generally internal, but might be used
    /// to fix a broken database.
    pub fn rebuild_event_indices<'a>(
        &'a self,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            // Erase all indices first
            self.db_event_ek_pk_index()?.clear(txn)?;
            self.db_event_ek_c_index()?.clear(txn)?;
            self.db_event_tag_index()?.clear(txn)?;
            self.db_hashtags()?.clear(txn)?;

            let loop_txn = self.env.read_txn()?;
            for result in self.db_events()?.iter(&loop_txn)? {
                let (_key, val) = result?;
                let event = Event::read_from_buffer(val)?;

                // If giftwrap, index the inner rumor instead
                let mut eventptr: &Event = &event;
                let rumor: Event;
                if let Some(r) = self.switch_to_rumor(&event, txn)? {
                    rumor = r;
                    eventptr = &rumor;
                }

                self.write_event_ek_pk_index(
                    eventptr.id,
                    eventptr.kind,
                    eventptr.pubkey,
                    Some(txn),
                )?;
                self.write_event_ek_c_index(
                    eventptr.id,
                    eventptr.kind,
                    eventptr.created_at,
                    Some(txn),
                )?;
                self.write_event_tag_index(eventptr, Some(txn))?;
                for hashtag in event.hashtags() {
                    if hashtag.is_empty() {
                        continue;
                    } // upstream bug
                    self.add_hashtag(&hashtag, event.id, Some(txn))?;
                }
            }
            Ok(())
        };

        match rw_txn {
            Some(txn) => {
                f(txn)?;
            }
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    pub fn rebuild_event_tags_index<'a>(
        &'a self,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            // Erase the index first
            self.db_event_tag_index()?.clear(txn)?;

            let loop_txn = self.env.read_txn()?;
            for result in self.db_events()?.iter(&loop_txn)? {
                let (_key, val) = result?;
                let event = Event::read_from_buffer(val)?;
                self.write_event_tag_index(&event, Some(txn))?;
            }
            Ok(())
        };

        match rw_txn {
            Some(txn) => {
                f(txn)?;
            }
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    /// Read person lists
    pub fn read_person_lists(
        &self,
        pubkey: &PublicKey,
    ) -> Result<HashMap<PersonList, bool>, Error> {
        self.read_person_lists2(pubkey)
    }

    /// Write person lists
    pub fn write_person_lists<'a>(
        &'a self,
        pubkey: &PublicKey,
        lists: HashMap<PersonList, bool>,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.write_person_lists2(pubkey, lists, rw_txn)
    }

    /// Get people in a person list
    pub fn get_people_in_list(&self, list: PersonList) -> Result<Vec<(PublicKey, bool)>, Error> {
        self.get_people_in_list2(list)
    }

    pub fn get_people_in_all_followed_lists(&self) -> Result<Vec<PublicKey>, Error> {
        self.get_people_in_all_followed_lists2()
    }

    /// Empty a person list
    #[inline]
    pub fn clear_person_list<'a>(
        &'a self,
        list: PersonList,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            self.clear_person_list2(list, Some(txn))?;
            let now = Unixtime::now().unwrap();
            if let Some(mut metadata) = self.get_person_list_metadata(list)? {
                metadata.last_edit_time = now;
                metadata.len = 0;
                self.set_person_list_metadata(list, &metadata, Some(txn))?;
            }
            Ok(())
        };

        match rw_txn {
            Some(txn) => {
                f(txn)?;
            }
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    /// Mark everybody in a list as private
    pub fn set_all_people_in_list_to_private<'a>(
        &'a self,
        list: PersonList,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            let people = self.get_people_in_list(list)?;
            for (pk, _) in &people {
                self.add_person_to_list(pk, list, false, Some(txn))?
            }
            Ok(())
        };

        match rw_txn {
            Some(txn) => {
                f(txn)?;
            }
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    /// Is a person in a list?
    pub fn is_person_in_list(&self, pubkey: &PublicKey, list: PersonList) -> Result<bool, Error> {
        let map = self.read_person_lists(pubkey)?;
        Ok(map.contains_key(&list))
    }

    /// Is the person in any list we subscribe to?
    pub fn is_person_subscribed_to(&self, pubkey: &PublicKey) -> Result<bool, Error> {
        let map = self.read_person_lists(pubkey)?;
        Ok(map.iter().any(|l| l.0.subscribe()))
    }

    /// Add a person to a list
    pub fn add_person_to_list<'a>(
        &'a self,
        pubkey: &PublicKey,
        list: PersonList,
        public: bool,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            let mut map = self.read_person_lists(pubkey)?;
            let had = map.contains_key(&list);
            map.insert(list, public);
            self.write_person_lists(pubkey, map, Some(txn))?;
            let now = Unixtime::now().unwrap();
            if let Some(mut metadata) = self.get_person_list_metadata(list)? {
                if !had {
                    metadata.len += 1;
                }
                metadata.last_edit_time = now;
                self.set_person_list_metadata(list, &metadata, Some(txn))?;
            }

            Ok(())
        };

        match rw_txn {
            Some(txn) => {
                f(txn)?;
            }
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    /// Remove a person from a list
    pub fn remove_person_from_list<'a>(
        &'a self,
        pubkey: &PublicKey,
        list: PersonList,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            let mut map = self.read_person_lists(pubkey)?;
            let had = map.contains_key(&list);
            map.remove(&list);
            self.write_person_lists(pubkey, map, Some(txn))?;
            let now = Unixtime::now().unwrap();
            if let Some(mut metadata) = self.get_person_list_metadata(list)? {
                if had && metadata.len > 0 {
                    metadata.len -= 1;
                }
                metadata.last_edit_time = now;
                self.set_person_list_metadata(list, &metadata, Some(txn))?;
            }
            Ok(())
        };

        match rw_txn {
            Some(txn) => {
                f(txn)?;
            }
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    /// Rebuild relationships
    pub fn rebuild_relationships<'a>(
        &'a self,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        tracing::info!("Rebuilding relationships...");

        let f = |txn: &mut RwTxn<'a>| -> Result<(), Error> {
            // Iterate through all events
            let loop_txn = self.env.read_txn()?;
            for result in self.db_events()?.iter(&loop_txn)? {
                let (_key, val) = result?;
                let event = Event::read_from_buffer(val)?;
                crate::process::process_relationships_of_event(&event, Some(txn))?;
            }
            self.set_flag_rebuild_relationships_needed(false, Some(txn))?;
            Ok(())
        };

        match rw_txn {
            Some(txn) => {
                f(txn)?;
            }
            None => {
                let mut txn = self.env.write_txn()?;
                f(&mut txn)?;
                txn.commit()?;
            }
        };

        Ok(())
    }

    pub fn write_nip46server<'a>(
        &'a self,
        server: &Nip46Server,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.write_nip46server2(server, rw_txn)
    }

    pub fn read_nip46server(&self, pubkey: PublicKey) -> Result<Option<Nip46Server>, Error> {
        self.read_nip46server2(pubkey)
    }

    pub fn read_all_nip46servers(&self) -> Result<Vec<Nip46Server>, Error> {
        self.read_all_nip46servers2()
    }

    pub fn delete_nip46server<'a>(
        &'a self,
        pubkey: PublicKey,
        rw_txn: Option<&mut RwTxn<'a>>,
    ) -> Result<(), Error> {
        self.delete_nip46server2(pubkey, rw_txn)
    }
}
