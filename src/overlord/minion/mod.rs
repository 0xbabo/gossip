mod handle_bus;
mod handle_websocket;
mod subscription;

use crate::comms::BusMessage;
use crate::db::DbRelay;
use crate::error::Error;
use crate::globals::GLOBALS;
use futures::{SinkExt, StreamExt};
use futures_util::stream::{SplitSink, SplitStream};
use http::Uri;
use nostr_types::{
    EventKind, Filter, IdHex, PublicKeyHex, RelayInformationDocument, Unixtime, Url,
};
use subscription::Subscriptions;
use tokio::net::TcpStream;
use tokio::select;
use tokio::sync::broadcast::Receiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, trace, warn};
use tungstenite::protocol::{Message as WsMessage, WebSocketConfig};

pub struct Minion {
    url: Url,
    to_overlord: UnboundedSender<BusMessage>,
    from_overlord: Receiver<BusMessage>,
    dbrelay: DbRelay,
    nip11: Option<RelayInformationDocument>,
    stream: Option<SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>>,
    sink: Option<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, WsMessage>>,
    subscriptions: Subscriptions,
    next_events_subscription_id: u32,
}

impl Minion {
    pub async fn new(url: Url) -> Result<Minion, Error> {
        if !url.is_valid_relay_url() {
            return Err(Error::InvalidUrl(url.inner().to_owned()));
        }

        let to_overlord = GLOBALS.to_overlord.clone();
        let from_overlord = GLOBALS.to_minions.subscribe();
        let dbrelay = match DbRelay::fetch_one(&url).await? {
            Some(dbrelay) => dbrelay,
            None => {
                let dbrelay = DbRelay::new(url.inner().to_owned())?;
                DbRelay::insert(dbrelay.clone()).await?;
                dbrelay
            }
        };

        Ok(Minion {
            url,
            to_overlord,
            from_overlord,
            dbrelay,
            nip11: None,
            stream: None,
            sink: None,
            subscriptions: Subscriptions::new(),
            next_events_subscription_id: 0,
        })
    }
}

impl Minion {
    pub async fn handle(&mut self) {
        // Catch errors, Return nothing.
        if let Err(e) = self.handle_inner().await {
            error!("{}: ERROR: {}", &self.url, e);

            // Bump the failure count for the relay.
            self.dbrelay.failure_count += 1;
            if let Err(e) = DbRelay::update(self.dbrelay.clone()).await {
                error!("{}: ERROR bumping relay failure count: {}", &self.url, e);
            }
        }

        debug!("{}: minion exiting", self.url);
    }

    async fn handle_inner(&mut self) -> Result<(), Error> {
        info!("{}: Minion handling started", &self.url);

        // Connect to the relay
        let websocket_stream = {
            let uri: http::Uri = self.url.inner().parse::<Uri>()?;
            let authority = uri.authority().ok_or(Error::UrlHasNoHostname)?.as_str();
            let host = authority
                .find('@')
                .map(|idx| authority.split_at(idx + 1).1)
                .unwrap_or_else(|| authority);
            if host.is_empty() {
                return Err(Error::UrlHasEmptyHostname);
            }

            let request_nip11_future = reqwest::Client::new()
                .get(format!("https://{}", host))
                .header("Host", host)
                .header("Accept", "application/nostr+json")
                .send();

            // Read NIP-11 information
            if let Ok(response) =
                tokio::time::timeout(std::time::Duration::new(15, 0), request_nip11_future).await?
            {
                match response.json::<RelayInformationDocument>().await {
                    Ok(nip11) => {
                        info!("{}: {}", &self.url, nip11);
                        self.nip11 = Some(nip11);
                    }
                    Err(e) => {
                        error!("{}: Unable to parse response as NIP-11: {}", &self.url, e);
                    }
                }
            }

            let key: [u8; 16] = rand::random();

            let req = http::request::Request::builder()
                .method("GET")
                .header("Host", host)
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Version", "13")
                .header("Sec-WebSocket-Key", base64::encode(key))
                .uri(uri)
                .body(())?;

            let config: WebSocketConfig = WebSocketConfig {
                max_send_queue: None,
                max_message_size: Some(1024 * 1024 * 16), // their default is 64 MiB, I choose 16 MiB
                max_frame_size: Some(1024 * 1024 * 16),   // their default is 16 MiB.
                accept_unmasked_frames: true,             // default is false which is the standard
            };

            let (websocket_stream, _response) = tokio::time::timeout(
                std::time::Duration::new(15, 0),
                tokio_tungstenite::connect_async_with_config(req, Some(config)),
            )
            .await??;
            info!("{}: Connected", &self.url);

            websocket_stream
        };

        let (sink, stream) = websocket_stream.split();
        self.stream = Some(stream);
        self.sink = Some(sink);

        // Bump the success count for the relay
        {
            let now = Unixtime::now().unwrap().0 as u64;
            DbRelay::update_success(self.dbrelay.url.clone(), now).await?
        }

        // Tell the overlord we are ready to receive commands
        self.tell_overlord_we_are_ready().await?;

        'relayloop: loop {
            match self.loop_handler().await {
                Ok(keepgoing) => {
                    if !keepgoing {
                        break 'relayloop;
                    }
                }
                Err(e) => {
                    // Log them and keep going
                    error!("{}: {}", &self.url, e);
                }
            }
        }

        Ok(())
    }

    async fn loop_handler(&mut self) -> Result<bool, Error> {
        let mut keepgoing: bool = true;

        let ws_stream = self.stream.as_mut().unwrap();
        let ws_sink = self.sink.as_mut().unwrap();

        let mut timer = tokio::time::interval(std::time::Duration::new(55, 0));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        timer.tick().await; // use up the first immediate tick.

        select! {
            _ = timer.tick() => {
                ws_sink.send(WsMessage::Ping(vec![])).await?;
            },
            ws_message = ws_stream.next() => {
                let ws_message = match ws_message {
                    Some(m) => m,
                    None => return Ok(false), // probably connection reset
                }?;

                trace!("{}: Handling message", &self.url);
                match ws_message {
                    WsMessage::Text(t) => {
                        // MAYBE FIXME, spawn a separate task here so that
                        // we don't miss ping ticks
                        self.handle_nostr_message(t).await?;
                        // FIXME: some errors we should probably bail on.
                        // For now, try to continue.
                    },
                    WsMessage::Binary(_) => warn!("{}, Unexpected binary message", &self.url),
                    WsMessage::Ping(x) => ws_sink.send(WsMessage::Pong(x)).await?,
                    WsMessage::Pong(_) => { }, // we just ignore pongs
                    WsMessage::Close(_) => keepgoing = false,
                    WsMessage::Frame(_) => warn!("{}: Unexpected frame message", &self.url),
                }
            },
            bus_message = self.from_overlord.recv() => {
                let bus_message = match bus_message {
                    Ok(bm) => bm,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Ok(false);
                    },
                    Err(e) => return Err(e.into())
                };
                #[allow(clippy::collapsible_if)]
                if bus_message.target == self.url.inner() {
                    self.handle_bus_message(bus_message).await?;
                } else if &*bus_message.target == "all" {
                    if &*bus_message.kind == "shutdown" {
                        info!("{}: Websocket listener shutting down", &self.url);
                        keepgoing = false;
                    }
                }
            },
        }

        Ok(keepgoing)
    }

    async fn tell_overlord_we_are_ready(&self) -> Result<(), Error> {
        self.to_overlord.send(BusMessage {
            target: "overlord".to_string(),
            kind: "minion_is_ready".to_string(),
            json_payload: "".to_owned(),
        })?;

        Ok(())
    }

    // Create or replace the following subscription
    async fn upsert_following(&mut self, pubkeys: Vec<PublicKeyHex>) -> Result<(), Error> {
        let websocket_sink = self.sink.as_mut().unwrap();

        if pubkeys.is_empty() {
            if let Some(sub) = self.subscriptions.get("following") {
                // Close the subscription
                let wire = serde_json::to_string(&sub.close_message())?;
                websocket_sink.send(WsMessage::Text(wire.clone())).await?;

                // Remove the subscription from the map
                self.subscriptions.remove("following");
            }

            // Since pubkeys is empty, nothing to subscribe to.
            return Ok(());
        }

        // Compute how far to look back
        let (feed_since, special_since) = {
            // Get related settings
            let (overlap, feed_chunk) = {
                let settings = GLOBALS.settings.read().await.clone();
                (settings.overlap, settings.feed_chunk)
            };

            /*
            // Find the oldest 'last_fetched' among the 'person_relay' table.
            // Null values will come through as 0.
            let mut special_since: i64 =
                DbPersonRelay::fetch_oldest_last_fetched(&pubkeys, &self.url.0).await? as i64;
            */

            // Start with where we left off, the time we last got something from
            // this relay.
            let mut special_since: i64 = match self.dbrelay.last_success_at {
                Some(u) => u as i64,
                None => 0,
            };

            // Subtract overlap to avoid gaps due to clock sync and event
            // propogation delay
            special_since -= overlap as i64;

            // Some relays don't like dates before 1970.  Hell, we don't need anything before 2020:a
            if special_since < 1577836800 {
                special_since = 1577836800;
            }

            // For feed related events, don't look back more than one feed_chunk ago
            let one_feedchunk_ago = Unixtime::now().unwrap().0 - feed_chunk as i64;
            let feed_since = special_since.max(one_feedchunk_ago);

            (Unixtime(feed_since), Unixtime(special_since))
        };

        // Create the author filter
        let mut feed_filter: Filter = Filter::new();
        for pk in pubkeys.iter() {
            feed_filter.add_author(pk, None);
        }
        feed_filter.add_event_kind(EventKind::TextNote);
        feed_filter.add_event_kind(EventKind::Reaction);
        feed_filter.add_event_kind(EventKind::EventDeletion);
        feed_filter.since = Some(feed_since);

        debug!(
            "{}: Feed Filter: {} authors",
            &self.url,
            feed_filter.authors.len()
        );

        // Create the lookback filter
        let mut special_filter: Filter = Filter::new();
        for pk in pubkeys.iter() {
            special_filter.add_author(pk, None);
        }
        special_filter.add_event_kind(EventKind::Metadata);
        special_filter.add_event_kind(EventKind::RecommendRelay);
        special_filter.add_event_kind(EventKind::ContactList);
        special_filter.add_event_kind(EventKind::RelaysList);
        special_filter.since = Some(special_since);
        debug!(
            "{}: Special Filter: {} authors",
            &self.url,
            special_filter.authors.len()
        );

        // Get the subscription
        let req_message = if self.subscriptions.has("following") {
            let sub = self.subscriptions.get_mut("following").unwrap();
            let vec: &mut Vec<Filter> = sub.get_mut();
            vec.clear();
            vec.push(feed_filter);
            vec.push(special_filter);
            sub.req_message()
        } else {
            self.subscriptions
                .add("following", vec![feed_filter, special_filter]);
            self.subscriptions.get("following").unwrap().req_message()
        };

        // Subscribe (or resubscribe) to the subscription
        let wire = serde_json::to_string(&req_message)?;
        websocket_sink.send(WsMessage::Text(wire.clone())).await?;

        trace!("{}: Sent {}", &self.url, &wire);

        Ok(())
    }

    async fn get_events(&mut self, ids: Vec<IdHex>) -> Result<(), Error> {
        if ids.is_empty() {
            return Ok(());
        }

        // create the filter
        let mut filter = Filter::new();
        filter.ids = ids;

        debug!("{}: Event Filter: {} events", &self.url, filter.ids.len());

        // create a handle for ourselves
        let handle = format!("temp_events_{}", self.next_events_subscription_id);
        self.next_events_subscription_id += 1;

        // save the subscription
        self.subscriptions.add(&handle, vec![filter]);

        // get the request message
        let req_message = self.subscriptions.get(&handle).unwrap().req_message();

        // Subscribe on the relay
        let websocket_sink = self.sink.as_mut().unwrap();
        let wire = serde_json::to_string(&req_message)?;
        websocket_sink.send(WsMessage::Text(wire.clone())).await?;

        trace!("{}: Sent {}", &self.url, &wire);

        Ok(())
    }

    async fn temp_subscribe_metadata(&mut self, pubkeyhex: PublicKeyHex) -> Result<(), Error> {
        let handle = format!("temp_subscribe_metadata_{}", &pubkeyhex.0);
        let filter = Filter {
            authors: vec![pubkeyhex],
            kinds: vec![EventKind::Metadata, EventKind::ContactList],
            // FIXME: we could probably get a since-last-fetched-their-metadata here.
            //        but relays should just return the lastest of these.
            ..Default::default()
        };
        self.subscribe(vec![filter], &handle).await
    }

    #[allow(dead_code)]
    async fn subscribe(&mut self, filters: Vec<Filter>, handle: &str) -> Result<(), Error> {
        let req_message = if self.subscriptions.has(handle) {
            let sub = self.subscriptions.get_mut(handle).unwrap();
            *sub.get_mut() = filters;
            sub.req_message()
        } else {
            self.subscriptions.add(handle, filters);
            self.subscriptions.get(handle).unwrap().req_message()
        };
        let wire = serde_json::to_string(&req_message)?;
        let websocket_sink = self.sink.as_mut().unwrap();
        websocket_sink.send(WsMessage::Text(wire.clone())).await?;
        trace!("{}: Sent {}", &self.url, &wire);
        Ok(())
    }

    #[allow(dead_code)]
    async fn unsubscribe(&mut self, handle: &str) -> Result<(), Error> {
        if !self.subscriptions.has(handle) {
            return Ok(());
        }
        let close_message = self.subscriptions.get(handle).unwrap().close_message();
        let wire = serde_json::to_string(&close_message)?;
        let websocket_sink = self.sink.as_mut().unwrap();
        websocket_sink.send(WsMessage::Text(wire.clone())).await?;
        trace!("{}: Sent {}", &self.url, &wire);
        self.subscriptions.remove(handle);
        Ok(())
    }
}
