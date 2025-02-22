#![cfg_attr(not(debug_assertions), windows_subsystem = "console")]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
// TEMPORARILY
#![allow(clippy::uninlined_format_args)]

//! Gossip lib is the core of the gossip nostr client.  The canonical binary crate is
//! `gossip_bin`.
//!
//! This library has been separated so that people can attach different non-canonical
//! user interfaces on top of this core.
//!
//! Because of the history of this API, it may be a bit clunky. But we will work to
//! improve that. Please submit PRs if you want to help. This interface will change
//! fairly rapidly for a while and then settle down.
//!
//! # Using gossip-lib
//!
//! To use gossip-lib, depend on it in your Cargo.toml
//!
//! ```rust.ignore
//! gossip-lib = { git = "https://github.com/mikedilger/gossip" }
//! ```
//!
//! You may specify optional features including:
//!
//! * Choose between `rustls-tls` and `native-tls`
//! * `lang-cjk` to include Chinese, Japanese, and Korean fonts (which grow the size significantly)
//!
//! # Gossip Startup
//!
//! Gossip starts up in three phases.
//!
//! The first phase happens at static initialization.
//! The globally available GLOBALS variable is initialized when first accessed, lazily.
//! You don't have to do anything special to make this happen, and you can start using
//! `GLOBALS` whenever you wish.
//!
//! The second phase is where you have to initialize a few things such as `Storage::init()`.
//! There may be others.
//!
//! The third phase is creating and starting the `Overlord`. This needs to be spawned on
//! a rust async executor such as `tokio`. See [Overlord::new](crate::Overlord::new) for the
//! details of how to start it. The overlord will start anything else that needs starting,
//! and will manage connections to relays.
//!
//! # User Interfaces
//!
//! The canonical gossip user interface is egui-based, and is thus immediate mode. It runs on
//! the main thread and is not asynchronous. Every call it makes must return immediately so that
//! it can paint the next frame (which may happen rapidly when videos are playing or scrolling
//! is animating) and not stall the user experience. For this reason, the `Overlord` can be sent
//! messages through a global message queue `GLOBALS.to_overlord`.
//!
//! But if your UI is asynchronous, you're probably better off calling `Overlord` functions
//! so that you can know when they complete.  Generally they don't return anything of interest,
//! but will return an `Error` if that happens.  The result instead appears as a side-effect
//! either in GLOBALS data or in the database.
//!
//! # Storage
//!
//! Besides talking to the `Overlord`, the most common thing a front-end needs to do is interact
//! with the storage engine. In some cases, the `Overlord` has more complex code for doing this,
//! but in many cases, you can interact with `GLOBALS.storage` directly.

mod about;
pub use about::About;

/// Defines messages sent to the overlord
pub mod comms;

mod delegation;
pub use delegation::Delegation;

mod dm_channel;
pub use dm_channel::{DmChannel, DmChannelData};

// direct quick-temporary communication with relays, without overlord/minion involvement
pub mod direct;

mod error;
pub use error::{Error, ErrorKind};

mod feed;
pub use feed::{Feed, FeedKind};

mod fetcher;
pub use fetcher::Fetcher;

mod filter;

mod globals;
pub use globals::{Globals, ZapState, GLOBALS};

mod gossip_identity;
pub use gossip_identity::GossipIdentity;

mod media;
pub use media::Media;

/// Rendering various names of users
pub mod names;

/// nip05 handling
pub mod nip05;

#[allow(dead_code)]
pub mod nip46;
pub use nip46::{Nip46Server, Nip46UnconnectedServer};

mod overlord;
pub use overlord::Overlord;

mod people;
pub use people::{People, Person, PersonList, PersonListMetadata};

mod person_relay;
pub use person_relay::PersonRelay;

/// Processing incoming events
pub mod process;

mod profile;

mod relationship;

mod relay;
pub use relay::Relay;

mod relay_picker_hooks;
pub use relay_picker_hooks::Hooks;

mod status;
pub use status::StatusQueue;

mod storage;
pub use storage::types::*;
pub use storage::Storage;

mod tags;

#[macro_use]
extern crate lazy_static;

/// The USER_AGENT string for gossip that it (may) use when fetching HTTP resources and
/// when connecting to relays
pub static USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

use std::ops::DerefMut;

/// Initialize gossip-lib
pub fn init() -> Result<(), Error> {
    // Initialize storage
    GLOBALS.storage.init()?;

    // Load signer from settings
    GLOBALS.identity.load()?;

    // Load delegation tag
    GLOBALS.delegation.load()?;

    // If we have a key but have not unlocked it
    if GLOBALS.identity.has_private_key() && !GLOBALS.identity.is_unlocked() {
        // If we need to rebuild relationships
        if GLOBALS.storage.get_flag_rebuild_relationships_needed() {
            GLOBALS
                .wait_for_login
                .store(true, std::sync::atomic::Ordering::Relaxed);
            GLOBALS
                .wait_for_data_migration
                .store(true, std::sync::atomic::Ordering::Relaxed);
        } else if GLOBALS.storage.read_setting_login_at_startup() {
            GLOBALS
                .wait_for_login
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    Ok(())
}

/// Shutdown gossip-lib
pub fn shutdown() -> Result<(), Error> {
    // Sync storage again
    if let Err(e) = GLOBALS.storage.sync() {
        tracing::error!("{}", e);
    } else {
        tracing::info!("LMDB synced.");
    }

    Ok(())
}

/// Run gossip-lib as an async
pub async fn run() {
    // Steal `tmp_overlord_receiver` from the GLOBALS, and give it to a new Overlord
    let overlord_receiver = {
        let mut mutex_option = GLOBALS.tmp_overlord_receiver.lock().await;
        mutex_option.deref_mut().take()
    }
    .unwrap();

    // Run the overlord
    let mut overlord = Overlord::new(overlord_receiver);
    overlord.run().await;
}
