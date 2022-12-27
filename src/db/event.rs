use crate::error::Error;
use crate::globals::GLOBALS;
use nostr_types::{IdHex, PublicKeyHex};
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DbEvent {
    pub id: IdHex,
    pub raw: String,
    pub pubkey: PublicKeyHex,
    pub created_at: i64,
    pub kind: u64,
    pub content: String,
    pub ots: Option<String>,
}

impl DbEvent {
    pub async fn fetch(criteria: Option<&str>) -> Result<Vec<DbEvent>, Error> {
        let sql = "SELECT id, raw, pubkey, created_at, kind, content, ots FROM event".to_owned();
        let sql = match criteria {
            None => sql,
            Some(crit) => format!("{} WHERE {}", sql, crit),
        };

        let output: Result<Vec<DbEvent>, Error> = spawn_blocking(move || {
            let maybe_db = GLOBALS.db.blocking_lock();
            let db = maybe_db.as_ref().unwrap();

            let mut stmt = db.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok(DbEvent {
                    id: IdHex(row.get(0)?),
                    raw: row.get(1)?,
                    pubkey: PublicKeyHex(row.get(2)?),
                    created_at: row.get(3)?,
                    kind: row.get(4)?,
                    content: row.get(5)?,
                    ots: row.get(6)?,
                })
            })?;

            let mut output: Vec<DbEvent> = Vec::new();
            for row in rows {
                output.push(row?);
            }
            Ok(output)
        })
        .await?;

        output
    }

    pub async fn fetch_by_ids(ids: Vec<IdHex>) -> Result<Vec<DbEvent>, Error> {
        if ids.is_empty() {
            return Ok(vec![]);
        }

        let sql = format!(
            "SELECT id, raw, pubkey, created_at, kind, content, ots FROM event WHERE id IN ({})",
            repeat_vars(ids.len())
        );

        let output: Result<Vec<DbEvent>, Error> = spawn_blocking(move || {
            let maybe_db = GLOBALS.db.blocking_lock();
            let db = maybe_db.as_ref().unwrap();

            let id_strings: Vec<String> = ids.iter().map(|p| p.0.clone()).collect();

            let mut stmt = db.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(id_strings), |row| {
                Ok(DbEvent {
                    id: IdHex(row.get(0)?),
                    raw: row.get(1)?,
                    pubkey: PublicKeyHex(row.get(2)?),
                    created_at: row.get(3)?,
                    kind: row.get(4)?,
                    content: row.get(5)?,
                    ots: row.get(6)?,
                })
            })?;

            let mut output: Vec<DbEvent> = Vec::new();
            for row in rows {
                output.push(row?);
            }
            Ok(output)
        })
        .await?;

        output
    }

    pub async fn fetch_latest_metadata() -> Result<Vec<DbEvent>, Error> {
        // THIS SQL MIGHT WORK, NEEDS REVIEW
        let sql = "SELECT id, LAST_VALUE(raw) OVER (ORDER BY created_at desc) as last_raw, pubkey, LAST_VALUE(created_at) OVER (ORDER BY created_at desc), kind, content, ots FROM event WHERE kind=0".to_owned();

        let output: Result<Vec<DbEvent>, Error> = spawn_blocking(move || {
            let maybe_db = GLOBALS.db.blocking_lock();
            let db = maybe_db.as_ref().unwrap();

            let mut stmt = db.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok(DbEvent {
                    id: IdHex(row.get(0)?),
                    raw: row.get(1)?,
                    pubkey: PublicKeyHex(row.get(2)?),
                    created_at: row.get(3)?,
                    kind: row.get(4)?,
                    content: row.get(5)?,
                    ots: row.get(6)?,
                })
            })?;

            let mut output: Vec<DbEvent> = Vec::new();
            for row in rows {
                output.push(row?);
            }
            Ok(output)
        })
        .await?;

        output
    }

    pub async fn insert(event: DbEvent) -> Result<(), Error> {
        let sql = "INSERT OR IGNORE INTO event (id, raw, pubkey, created_at, kind, content, ots) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

        spawn_blocking(move || {
            let maybe_db = GLOBALS.db.blocking_lock();
            let db = maybe_db.as_ref().unwrap();

            let mut stmt = db.prepare(sql)?;
            stmt.execute((
                &event.id.0,
                &event.raw,
                &event.pubkey.0,
                &event.created_at,
                &event.kind,
                &event.content,
                &event.ots,
            ))?;
            Ok::<(), Error>(())
        })
        .await??;

        Ok(())
    }

    #[allow(dead_code)]
    pub async fn delete(criteria: &str) -> Result<(), Error> {
        let sql = format!("DELETE FROM event WHERE {}", criteria);

        spawn_blocking(move || {
            let maybe_db = GLOBALS.db.blocking_lock();
            let db = maybe_db.as_ref().unwrap();
            db.execute(&sql, [])?;
            Ok::<(), Error>(())
        })
        .await??;

        Ok(())
    }

    #[allow(dead_code)]
    pub async fn get_author(id: IdHex) -> Result<Option<PublicKeyHex>, Error> {
        let sql = "SELECT pubkey FROM event WHERE id=?";

        spawn_blocking(move || {
            let maybe_db = GLOBALS.db.blocking_lock();
            let db = maybe_db.as_ref().unwrap();
            let mut stmt = db.prepare(sql)?;
            let mut rows = stmt.query_map([id.0], |row| row.get(0))?;
            if let Some(row) = rows.next() {
                return Ok(Some(PublicKeyHex(row?)));
            }
            Ok(None)
        })
        .await?
    }
}

fn repeat_vars(count: usize) -> String {
    assert_ne!(count, 0);
    let mut s = "?,".repeat(count);
    // Remove trailing comma
    s.pop();
    s
}
