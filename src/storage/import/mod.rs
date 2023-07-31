mod legacy;
use super::Storage;
use crate::error::Error;
use crate::settings::Settings;
use crate::ui::ThemeVariant;
use nostr_types::{EncryptedPrivateKey, PublicKey};
use rusqlite::Connection;

impl Storage {
    pub(super) fn import(&self) -> Result<(), Error> {
        tracing::info!("Importing SQLITE data into LMDB...");

        // Progress the legacy database to the endpoint first
        let mut db = legacy::init_database()?;
        legacy::setup_database(&mut db)?;
        tracing::info!("LDMB: setup");

        // local settings
        import_local_settings(&db, |epk: Option<EncryptedPrivateKey>, lcle: i64| {
            self.write_encrypted_private_key(&epk)?;
            self.write_last_contact_list_edit(lcle)
        })?;

        // old table "settings"
        // Copy settings (including local_settings)
        import_settings(&db, |settings: &Settings| self.write_settings(settings))?;

        // Mark migration level
        // TBD: self.write_migration_level(0)?;

        tracing::info!("Importing SQLITE data into LMDB: Done.");

        Ok(())
    }
}

fn import_local_settings<F>(db: &Connection, mut f: F) -> Result<(), Error>
where
    F: FnMut(Option<EncryptedPrivateKey>, i64) -> Result<(), Error>,
{
    // These are the only local settings we need to keep
    let sql = "SELECT encrypted_private_key, last_contact_list_edit FROM local_settings";
    let mut stmt = db.prepare(sql)?;
    let mut rows = stmt.raw_query();
    if let Some(row) = rows.next()? {
        let epk: Option<String> = row.get(0)?;
        let lcle: i64 = row.get(1)?;
        f(epk.map(EncryptedPrivateKey), lcle)?;
    }
    Ok(())
}

fn import_settings<F>(db: &Connection, mut f: F) -> Result<(), Error>
where
    F: FnMut(&Settings) -> Result<(), Error>,
{
    let numstr_to_bool = |s: String| -> bool { &s == "1" };

    let sql = "SELECT key, value FROM settings ORDER BY key";
    let mut stmt = db.prepare(sql)?;
    let mut rows = stmt.raw_query();
    let mut settings = Settings::default();
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        let value: String = row.get(1)?;
        match &*key {
            "feed_chunk" => {
                if let Ok(x) = value.parse::<u64>() {
                    settings.feed_chunk = x;
                }
            }
            "replies_chunk" => {
                if let Ok(x) = value.parse::<u64>() {
                    settings.replies_chunk = x;
                }
            }
            "overlap" => {
                if let Ok(x) = value.parse::<u64>() {
                    settings.overlap = x;
                }
            }
            "num_relays_per_person" => {
                if let Ok(x) = value.parse::<u8>() {
                    settings.num_relays_per_person = x;
                }
            }
            "max_relays" => {
                if let Ok(x) = value.parse::<u8>() {
                    settings.max_relays = x;
                }
            }
            "public_key" => {
                settings.public_key = match PublicKey::try_from_hex_string(&value) {
                    Ok(pk) => Some(pk),
                    Err(e) => {
                        tracing::error!("Public key in database is invalid or corrupt: {}", e);
                        None
                    }
                }
            }
            "max_fps" => {
                if let Ok(x) = value.parse::<u32>() {
                    settings.max_fps = x;
                }
            }
            "recompute_feed_periodically" => {
                settings.recompute_feed_periodically = numstr_to_bool(value)
            }
            "feed_recompute_interval_ms" => {
                if let Ok(x) = value.parse::<u32>() {
                    settings.feed_recompute_interval_ms = x;
                }
            }
            "pow" => {
                if let Ok(x) = value.parse::<u8>() {
                    settings.pow = x;
                }
            }
            "offline" => settings.offline = numstr_to_bool(value),
            "dark_mode" => settings.theme.dark_mode = numstr_to_bool(value),
            "follow_os_dark_mode" => settings.theme.follow_os_dark_mode = numstr_to_bool(value),
            "theme" => {
                for theme_variant in ThemeVariant::all() {
                    if &*value == theme_variant.name() {
                        settings.theme.variant = *theme_variant;
                        break;
                    }
                }
            }
            "set_client_tag" => settings.set_client_tag = numstr_to_bool(value),
            "set_user_agent" => settings.set_user_agent = numstr_to_bool(value),
            "override_dpi" => {
                if value.is_empty() {
                    settings.override_dpi = None;
                } else if let Ok(x) = value.parse::<u32>() {
                    settings.override_dpi = Some(x);
                }
            }
            "reactions" => settings.reactions = numstr_to_bool(value),
            "reposts" => settings.reposts = numstr_to_bool(value),
            "show_long_form" => settings.show_long_form = numstr_to_bool(value),
            "show_mentions" => settings.show_mentions = numstr_to_bool(value),
            "show_media" => settings.show_media = numstr_to_bool(value),
            "load_avatars" => settings.load_avatars = numstr_to_bool(value),
            "load_media" => settings.load_media = numstr_to_bool(value),
            "check_nip05" => settings.check_nip05 = numstr_to_bool(value),
            "direct_messages" => settings.direct_messages = numstr_to_bool(value),
            "automatically_fetch_metadata" => {
                settings.automatically_fetch_metadata = numstr_to_bool(value)
            }
            "delegatee_tag" => settings.delegatee_tag = value,
            "highlight_unread_events" => settings.highlight_unread_events = numstr_to_bool(value),
            "posting_area_at_top" => settings.posting_area_at_top = numstr_to_bool(value),
            "enable_zap_receipts" => settings.enable_zap_receipts = numstr_to_bool(value),
            _ => {}
        }
    }

    f(&settings)?;

    Ok(())
}
