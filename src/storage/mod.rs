mod import;

use crate::error::Error;
use crate::profile::Profile;
use crate::settings::Settings;
use lmdb::{
    Cursor, Database, DatabaseFlags, Environment, EnvironmentFlags, Transaction, WriteFlags,
};
use nostr_types::{EncryptedPrivateKey, Id, RelayUrl, Unixtime};
use speedy::{Readable, Writable};

const MAX_LMDB_KEY: usize = 511;
macro_rules! key {
    ($slice:expr) => {
        if $slice.len() > 511 {
            &$slice[..511]
        } else {
            $slice
        }
    };
}

pub struct Storage {
    env: Environment,

    // General database (settings, local_settings)
    general: Database,

    // Id:Url -> Unixtime
    event_seen_on_relay: Database,

    // Id -> ()
    event_viewed: Database,

    // Hashtag -> Id
    // (dup keys, so multiple Ids per hashtag)
    hashtags: Database,
}

impl Storage {
    pub fn new() -> Result<Storage, Error> {
        let mut builder = Environment::new();

        builder.set_flags(
            EnvironmentFlags::WRITE_MAP | // no nested transactions!
            EnvironmentFlags::NO_META_SYNC |
            EnvironmentFlags::MAP_ASYNC,
        );
        // builder.set_max_readers(126); // this is the default
        builder.set_max_dbs(32);

        // This has to be big enough for all the data.
        // Note that it is the size of the map in VIRTUAL address space,
        //   and that it doesn't all have to be paged in at the same time.
        builder.set_map_size(1048576 * 1024 * 2); // 2 GB (probably too small)

        let env = builder.open(&Profile::current()?.lmdb_dir)?;

        let general = env.create_db(None, DatabaseFlags::empty())?;

        let event_seen_on_relay =
            env.create_db(Some("event_seen_on_relay"), DatabaseFlags::empty())?;

        let event_viewed = env.create_db(Some("event_viewed"), DatabaseFlags::empty())?;

        let hashtags = env.create_db(
            Some("hashtags"),
            DatabaseFlags::DUP_SORT | DatabaseFlags::DUP_FIXED,
        )?;

        let storage = Storage {
            env,
            general,
            event_seen_on_relay,
            event_viewed,
            hashtags,
        };

        // If migration level is missing, we need to import from legacy sqlite
        match storage.read_migration_level()? {
            None => {
                // Import from sqlite
                storage.import()?;
            }
            Some(_level) => {
                // migrations happen here
            }
        }

        Ok(storage)
    }

    pub fn write_migration_level(&self, migration_level: u32) -> Result<(), Error> {
        let bytes = &migration_level.to_be_bytes();
        let mut txn = self.env.begin_rw_txn()?;
        txn.put(
            self.general,
            b"migration_level",
            &bytes,
            WriteFlags::empty(),
        )?;
        txn.commit()?;
        Ok(())
    }

    pub fn read_migration_level(&self) -> Result<Option<u32>, Error> {
        let txn = self.env.begin_ro_txn()?;
        match txn.get(self.general, b"migration_level") {
            Ok(bytes) => Ok(Some(u32::from_be_bytes(bytes[..4].try_into()?))),
            Err(lmdb::Error::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn write_encrypted_private_key(
        &self,
        epk: &Option<EncryptedPrivateKey>,
    ) -> Result<(), Error> {
        let bytes = epk.as_ref().map(|e| &e.0).write_to_vec()?;
        let mut txn = self.env.begin_rw_txn()?;
        txn.put(
            self.general,
            b"encrypted_private_key",
            &bytes,
            WriteFlags::empty(),
        )?;
        txn.commit()?;
        Ok(())
    }

    pub fn read_encrypted_private_key(&self) -> Result<Option<EncryptedPrivateKey>, Error> {
        let txn = self.env.begin_ro_txn()?;
        match txn.get(self.general, b"encrypted_private_key") {
            Ok(bytes) => {
                let os = Option::<String>::read_from_buffer(bytes)?;
                Ok(os.map(EncryptedPrivateKey))
            }
            Err(lmdb::Error::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn write_last_contact_list_edit(&self, when: i64) -> Result<(), Error> {
        let bytes = &when.to_be_bytes();
        let mut txn = self.env.begin_rw_txn()?;
        txn.put(
            self.general,
            b"last_contact_list_edit",
            &bytes,
            WriteFlags::empty(),
        )?;
        txn.commit()?;
        Ok(())
    }

    pub fn read_last_contact_list_edit(&self) -> Result<Option<i64>, Error> {
        let txn = self.env.begin_ro_txn()?;
        match txn.get(self.general, b"last_contact_list_edit") {
            Ok(bytes) => Ok(Some(i64::from_be_bytes(bytes[..8].try_into()?))),
            Err(lmdb::Error::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn write_settings(&self, settings: &Settings) -> Result<(), Error> {
        let bytes = settings.write_to_vec()?;
        let mut txn = self.env.begin_rw_txn()?;
        txn.put(self.general, b"settings", &bytes, WriteFlags::empty())?;
        txn.commit()?;
        Ok(())
    }

    pub fn read_settings(&self) -> Result<Option<Settings>, Error> {
        let txn = self.env.begin_ro_txn()?;
        match txn.get(self.general, b"settings") {
            Ok(bytes) => Ok(Some(Settings::read_from_buffer(bytes)?)),
            Err(lmdb::Error::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn add_event_seen_on_relay(
        &self,
        id: Id,
        url: &RelayUrl,
        when: Unixtime,
    ) -> Result<(), Error> {
        let bytes = &when.0.to_be_bytes();
        let mut key: Vec<u8> = id.as_slice().to_owned();
        let mut txn = self.env.begin_rw_txn()?;
        key.extend(url.0.as_bytes());
        key.truncate(MAX_LMDB_KEY);
        txn.put(self.event_seen_on_relay, &key, &bytes, WriteFlags::empty())?;
        txn.commit()?;
        Ok(())
    }

    pub fn get_event_seen_on_relay(&self, id: Id) -> Result<Vec<(RelayUrl, Unixtime)>, Error> {
        let start_key: Vec<u8> = id.as_slice().to_owned();
        let txn = self.env.begin_ro_txn()?;
        let mut cursor = txn.open_ro_cursor(self.event_seen_on_relay)?;
        let iter = cursor.iter_from(start_key.clone());
        let mut output: Vec<(RelayUrl, Unixtime)> = Vec::new();
        for result in iter {
            match result {
                Err(e) => return Err(e.into()),
                Ok((key, val)) => {
                    // Stop once we get to a different Id
                    if !key.starts_with(&start_key) {
                        break;
                    }
                    // Extract off the Url
                    let url = RelayUrl(std::str::from_utf8(&key[32..])?.to_owned());
                    let time = Unixtime(i64::from_be_bytes(val[..8].try_into()?));
                    output.push((url, time));
                }
            }
        }
        Ok(output)
    }

    pub fn mark_event_viewed(&self, id: Id) -> Result<(), Error> {
        let bytes = vec![];
        let mut txn = self.env.begin_rw_txn()?;
        txn.put(self.event_viewed, &id.as_ref(), &bytes, WriteFlags::empty())?;
        txn.commit()?;
        Ok(())
    }

    pub fn is_event_viewed(&self, id: Id) -> Result<bool, Error> {
        let txn = self.env.begin_ro_txn()?;
        match txn.get(self.event_viewed, &id.as_ref()) {
            Ok(_bytes) => Ok(true),
            Err(lmdb::Error::NotFound) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    pub fn add_hashtag(&self, hashtag: &String, id: Id) -> Result<(), Error> {
        let key = key!(hashtag.as_bytes());
        let bytes = id.as_slice().to_owned();
        let mut txn = self.env.begin_rw_txn()?;
        txn.put(self.hashtags, &key, &bytes, WriteFlags::empty())?;
        txn.commit()?;
        Ok(())
    }

    pub fn get_event_ids_with_hashtag(&self, hashtag: &String) -> Result<Vec<Id>, Error> {
        let key = key!(hashtag.as_bytes());
        let txn = self.env.begin_ro_txn()?;
        let mut cursor = txn.open_ro_cursor(self.hashtags)?;
        let iter = cursor.iter_from(key);
        let mut output: Vec<Id> = Vec::new();
        for result in iter {
            match result {
                Err(e) => return Err(e.into()),
                Ok((thiskey, val)) => {
                    // Stop once we get to a different key
                    if thiskey != key {
                        break;
                    }
                    let id = Id::read_from_buffer(val)?;
                    output.push(id);
                }
            }
        }
        Ok(output)
    }
}
