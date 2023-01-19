use crate::db::{DbPersonRelay, DbRelay};
use crate::error::Error;
use crate::globals::GLOBALS;
use crate::people::DbPerson;
use nostr_types::{Metadata, Nip05, Unixtime, Url};

// This updates the people map and the database with the result
#[allow(dead_code)]
pub async fn validate_nip05(person: DbPerson) -> Result<(), Error> {
    let now = Unixtime::now().unwrap();

    // invalid if their nip-05 is not set
    if person.metadata.is_none() || matches!(person.metadata, Some(Metadata { nip05: None, .. })) {
        GLOBALS
            .people
            .upsert_nip05_validity(&person.pubkey, None, false, now.0 as u64)
            .await?;
        return Ok(());
    }

    let metadata = person.metadata.as_ref().unwrap().to_owned();
    let nip05 = metadata.nip05.as_ref().unwrap().to_owned();

    // Split their DNS ID
    let (user, domain) = match parse_nip05(&nip05) {
        Ok(pair) => pair,
        Err(_) => {
            GLOBALS
                .people
                .upsert_nip05_validity(&person.pubkey, Some(nip05), false, now.0 as u64)
                .await?;
            return Ok(());
        }
    };

    // Fetch NIP-05
    let nip05file = match fetch_nip05(&user, &domain).await {
        Ok(content) => content,
        Err(e) => {
            tracing::error!("NIP-05 fetch issue with {}@{}", user, domain);
            return Err(e);
        }
    };

    // Check if the response matches their public key
    match nip05file.names.get(&user) {
        Some(pk) => {
            if *pk == person.pubkey {
                // Validated
                GLOBALS
                    .people
                    .upsert_nip05_validity(&person.pubkey, Some(nip05), true, now.0 as u64)
                    .await?;
            }
        }
        None => {
            // Failed
            GLOBALS
                .people
                .upsert_nip05_validity(&person.pubkey, Some(nip05), false, now.0 as u64)
                .await?;
        }
    }

    Ok(())
}

pub async fn get_and_follow_nip05(nip05: String) -> Result<(), Error> {
    // Split their DNS ID
    let (user, domain) = parse_nip05(&nip05)?;

    // Fetch NIP-05
    let nip05file = fetch_nip05(&user, &domain).await?;

    // Get their pubkey
    let pubkey = match nip05file.names.get(&user) {
        Some(pk) => pk,
        None => return Err(Error::Nip05KeyNotFound),
    };

    // Save person
    GLOBALS
        .people
        .upsert_nip05_validity(
            pubkey,
            Some(nip05.clone()),
            true,
            Unixtime::now().unwrap().0 as u64,
        )
        .await?;

    // Mark as followed
    GLOBALS.people.async_follow(pubkey, true).await?;

    tracing::info!("Followed {}", &nip05);

    // Set their relays
    let relays = match nip05file.relays.get(pubkey) {
        Some(relays) => relays,
        None => return Err(Error::Nip05RelaysNotFound),
    };
    for relay in relays.iter() {
        // Save relay
        let relay_url = Url::new(relay);
        if relay_url.is_valid_relay_url() {
            let db_relay = DbRelay::new(relay_url.inner().to_owned())?;
            DbRelay::insert(db_relay).await?;

            // Save person_relay
            DbPersonRelay::upsert_last_suggested_nip05(
                pubkey.clone(),
                relay.inner().to_owned(),
                Unixtime::now().unwrap().0 as u64,
            )
            .await?;
        }
    }

    tracing::info!("Setup {} relays for {}", relays.len(), &nip05);

    Ok(())
}

// returns user and domain
fn parse_nip05(nip05: &str) -> Result<(String, String), Error> {
    let mut parts: Vec<&str> = nip05.split('@').collect();

    // Add the underscore as a username if they just specified a domain name.
    if parts.len() == 1 {
        parts = Vec::from(["_", parts.first().unwrap()])
    }

    // Require two parts
    if parts.len() != 2 {
        Err(Error::InvalidDnsId)
    } else {
        let domain = parts.pop().unwrap();
        let user = parts.pop().unwrap();
        if domain.len() < 4 {
            // smallest non-TLD domain is like 't.co'
            return Err(Error::InvalidDnsId);
        }
        Ok((user.to_string(), domain.to_string()))
    }
}

async fn fetch_nip05(user: &str, domain: &str) -> Result<Nip05, Error> {
    let nip05_future = reqwest::Client::builder()
        .timeout(std::time::Duration::new(60, 0))
        .redirect(reqwest::redirect::Policy::none()) // see NIP-05
        .build()?
        .get(format!(
            "https://{}/.well-known/nostr.json?name={}",
            domain, user
        ))
        .header("Host", domain)
        .send();
    let response = nip05_future.await?;
    Ok(response.json::<Nip05>().await?)
}
