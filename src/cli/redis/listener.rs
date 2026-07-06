use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use async_graphql_value::ConstValue;
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use futures_util::{Stream, StreamExt, stream};
use redis::aio::PubSubSink;
use redis::streams::StreamId;
use tokio::sync::broadcast;
use tracing::{error, info, trace, warn};

use super::client::redis_value_to_const;
use crate::core::config::RedisPayloadType;
use crate::core::redis::{RedisListenerIO, decode_payload, decode_value_leaves};

const BROADCAST_CAPACITY: usize = 256;
const INITIAL_RECONNECT_DELAY_MS: u64 = 1000;
const MAX_RECONNECT_DELAY_MS: u64 = 30000;
const CLIENT_INIT_POLL_INTERVAL_MS: u64 = 100;
const XREAD_BLOCK_MS: u64 = 5000;
const XREAD_COUNT: u64 = 100;
/// Margin added on top of `XREAD_BLOCK_MS` when computing the connection's
/// response timeout, so that a `BLOCK` cycle that legitimately runs for the
/// full duration is never mistaken for a hung connection. Without this, the
/// `redis` crate's default response timeout (~500ms, far shorter than any
/// realistic `BLOCK` duration) fires mid-`BLOCK` and tears down the
/// connection every idle cycle.
const XREAD_RESPONSE_TIMEOUT_MARGIN_MS: u64 = 5000;

/// Applies +/-25% jitter to a backoff duration to prevent thundering herd
/// when multiple listeners reconnect at the same time.
fn jittered_delay_ms(backoff_ms: u64) -> u64 {
    let jitter: u64 = rand::random_range(0..=backoff_ms / 4);
    if rand::random_bool(0.5) {
        backoff_ms.saturating_sub(jitter)
    } else {
        backoff_ms.saturating_add(jitter)
    }
}

/// Classifies a `redis::RedisError` from an `XREAD` call as transient
/// (worth silently reconnecting and retrying) or permanent (retrying would
/// just repeat the same failure forever).
///
/// Transient errors are connection-level failures -- the socket dropped,
/// timed out, or the server refused the connection -- where a fresh
/// connection has a real chance of succeeding. Everything else (e.g. a
/// server error such as `WRONGTYPE` because `key` isn't a Stream, or a
/// RESP type-conversion error) is permanent: `last_id` never advances and
/// no amount of reconnecting will change the outcome, so the caller should
/// surface the error instead of retrying forever.
fn is_transient_redis_error(err: &redis::RedisError) -> bool {
    err.is_io_error()
        || err.is_timeout()
        || err.is_connection_dropped()
        || err.is_connection_refusal()
}

pub struct RedisListener {
    /// A lightweight, unconnected handle. Cloned to open the dedicated
    /// connections used by `subscribe()`'s background driver and by every
    /// `read_stream()` call.
    client: redis::Client,
    /// Wrapped in `Arc` (rather than a plain `DashMap`) so that
    /// `SubscriptionGuard` can hold its own handle to the map and clean up
    /// its entry on drop, independent of the lifetime of any particular
    /// `&self` borrow.
    senders: Arc<DashMap<String, broadcast::Sender<String>>>,
    /// The write half of the current Pub/Sub connection, produced by
    /// splitting `redis::aio::PubSub`. `None` while disconnected.
    /// `PubSubSink` is cheap to lock briefly: `subscribe()` (external
    /// callers) and the reconnect driver both take this lock only for the
    /// duration of issuing a `SUBSCRIBE`, never while awaiting messages.
    /// Wrapped in `Arc` for the same reason as `senders`: `SubscriptionGuard`
    /// needs it to issue a best-effort `UNSUBSCRIBE` after `subscribe()`
    /// itself has returned.
    sink: Arc<tokio::sync::Mutex<Option<PubSubSink>>>,
}

impl RedisListener {
    /// Create a new listener and spawn its background reconnect driver on
    /// the current Tokio runtime (if any). This does not connect
    /// synchronously; the Pub/Sub connection is established by the driver
    /// task.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn new(connection_url: &str) -> anyhow::Result<Arc<Self>> {
        let client = redis::Client::open(connection_url)
            .map_err(|e| anyhow!("Failed to create Redis client: {e}"))?;

        let listener = Arc::new(Self {
            client,
            senders: Arc::new(DashMap::new()),
            sink: Arc::new(tokio::sync::Mutex::new(None)),
        });

        let driver_listener = listener.clone();

        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    driver_listener.run_driver().await;
                });
            }
            Err(_) => {
                warn!(
                    "No Tokio runtime available at RedisListener construction; listener will not function without a running Tokio runtime"
                );
            }
        }

        Ok(listener)
    }

    async fn run_driver(self: Arc<Self>) {
        let mut backoff_ms = INITIAL_RECONNECT_DELAY_MS;

        loop {
            match self.client.get_async_pubsub().await {
                Ok(pubsub) => {
                    let (mut new_sink, mut new_stream) = pubsub.split();

                    // Re-subscribe to every channel a caller previously
                    // registered before publishing the sink, so `subscribe()`
                    // never races a caller into issuing a `SUBSCRIBE` against
                    // a connection that hasn't resubscribed the backlog yet.
                    for entry in self.senders.iter() {
                        if let Err(e) = new_sink.subscribe(entry.key().as_str()).await {
                            error!(
                                error = %e,
                                channel = %entry.key(),
                                "Failed to re-subscribe to Redis Pub/Sub channel after reconnection"
                            );
                        }
                    }

                    {
                        let mut sink_guard = self.sink.lock().await;
                        *sink_guard = Some(new_sink);
                    }

                    backoff_ms = INITIAL_RECONNECT_DELAY_MS;
                    info!("Redis Pub/Sub listener connection established");

                    loop {
                        if let Some(msg) = new_stream.next().await {
                            let channel = msg.get_channel_name().to_string();
                            match msg.get_payload::<String>() {
                                Ok(payload) => self.send_notification(&channel, payload),
                                Err(e) => warn!(
                                    error = %e,
                                    channel = %channel,
                                    "Failed to decode Redis Pub/Sub payload as a UTF-8 string"
                                ),
                            }
                        } else {
                            info!("Redis Pub/Sub connection closed, will reconnect");
                            break;
                        }
                    }

                    // The sink shares the same underlying connection as the
                    // stream we just fell off of; drop it so `subscribe()`
                    // callers wait for the next reconnection instead of
                    // writing to a dead connection.
                    let mut sink_guard = self.sink.lock().await;
                    *sink_guard = None;
                }
                Err(e) => {
                    error!(
                        error = %e,
                        "Failed to open Redis Pub/Sub connection"
                    );
                }
            }

            backoff_ms = (backoff_ms * 2).min(MAX_RECONNECT_DELAY_MS);
            let delay_ms = jittered_delay_ms(backoff_ms);
            warn!(
                delay_ms = delay_ms,
                "Reconnecting Redis Pub/Sub listener in {delay_ms}ms"
            );
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    async fn wait_for_sink(&self, timeout: Duration) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.sink.lock().await.is_some() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("Timeout waiting for Redis Pub/Sub listener to initialize");
            }
            tokio::time::sleep(Duration::from_millis(CLIENT_INIT_POLL_INTERVAL_MS)).await;
        }
    }

    fn send_notification(&self, channel: &str, payload: String) {
        if let Some(tx) = self.senders.get(channel) {
            match tx.send(payload) {
                Ok(n) => {
                    trace!(
                        channel = %channel,
                        receivers = n,
                        "Sent message to broadcast channel"
                    );
                }
                Err(_send_err) => {
                    trace!(
                        channel = %channel,
                        "Message dropped: no receivers for channel"
                    );
                }
            }
        } else {
            trace!(
                channel = %channel,
                "Message for untracked channel, ignoring"
            );
        }
    }

    /// Registers a new subscriber for `channel` in the `senders` map,
    /// returning the shared broadcast sender (call `.subscribe()` on it to
    /// get a receiver) together with a `SubscriptionGuard` that
    /// unregisters the channel once that receiver (and every other clone
    /// of it) is dropped.
    ///
    /// This only manages the in-process `senders`/broadcast bookkeeping;
    /// it never touches the Redis connection, so it works (and is tested)
    /// without a connected Pub/Sub sink.
    fn register_subscriber(&self, channel: &str) -> (broadcast::Sender<String>, SubscriptionGuard) {
        let tx = match self.senders.entry(channel.to_string()) {
            Entry::Occupied(e) => e.get().clone(),
            Entry::Vacant(e) => {
                let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
                e.insert(tx.clone());
                tx
            }
        };

        let guard = SubscriptionGuard {
            channel: channel.to_string(),
            senders: self.senders.clone(),
            sink: self.sink.clone(),
        };

        (tx, guard)
    }

    #[cfg(test)]
    fn senders_len(&self) -> usize {
        self.senders.len()
    }

    /// Builds a listener that never spawns the background reconnect driver
    /// and is never actually connected (`sink` stays `None` forever). Lets
    /// tests exercise the `senders`/`SubscriptionGuard` bookkeeping in
    /// `register_subscriber()` in isolation, without a live Redis server.
    #[cfg(test)]
    fn new_disconnected_for_test() -> Self {
        let client = redis::Client::open("redis://127.0.0.1:6379/0")
            .unwrap_or_else(|e| panic!("URL is statically valid: {e}"));
        Self {
            client,
            senders: Arc::new(DashMap::new()),
            sink: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }
}

/// Dropped when a Pub/Sub subscription stream (or the last clone of its
/// receiver) goes away. If this was the last live receiver for `channel`,
/// removes the corresponding `senders` entry and, best-effort, asks Redis
/// to stop delivering the channel.
///
/// Without this, a client that subscribes to a unique, disposable channel
/// name and then disconnects (channels are rendered from GraphQL arguments
/// via Mustache, see `@redis(operation: SUBSCRIBE)`) would leave its entry
/// behind forever: both this map and the server's Pub/Sub table would grow
/// without bound as an attacker (or just churn) mints new channel names.
///
/// Ordering: in `subscribe()`'s `stream::unfold` state tuple, this guard is
/// placed *after* the `broadcast::Receiver`. The unfold closure contains an
/// `.await`, so its captured locals are dropped in declaration order when
/// the generator itself is dropped -- meaning the receiver is dropped
/// first (decrementing `Sender::receiver_count()`) before this guard's
/// `Drop` runs and reads that count.
///
/// Race safety: `DashMap::remove_if`'s predicate runs while holding the
/// shard's write lock, so it can't race a concurrent `subscribe()` call for
/// the same channel. Either that call has already cloned a live receiver
/// (the predicate observes a non-zero count and skips removal), or it
/// hasn't reached the map yet and will simply `insert` a fresh `Sender`
/// right after this one is removed. A stale `Sender` from that brief
/// handover window has no receivers and is never looked up again once
/// replaced, so it's harmless.
struct SubscriptionGuard {
    channel: String,
    senders: Arc<DashMap<String, broadcast::Sender<String>>>,
    sink: Arc<tokio::sync::Mutex<Option<PubSubSink>>>,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        if self
            .senders
            .remove_if(&self.channel, |_, tx| tx.receiver_count() == 0)
            .is_none()
        {
            // Either another receiver is still alive, or a concurrent
            // `subscribe()` beat us to the lookup -- nothing to clean up.
            return;
        }

        let channel = self.channel.clone();
        let sink = self.sink.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    let mut sink_guard = sink.lock().await;
                    if let Some(sink) = sink_guard.as_mut()
                        && let Err(e) = sink.unsubscribe(&channel).await
                    {
                        warn!(
                            error = %e,
                            channel = %channel,
                            "Failed to UNSUBSCRIBE from Redis Pub/Sub channel after last subscriber disconnected"
                        );
                    }
                });
            }
            Err(_) => {
                warn!(
                    channel = %channel,
                    "No Tokio runtime available to send Redis UNSUBSCRIBE after last subscriber disconnected"
                );
            }
        }
    }
}

#[async_trait::async_trait]
impl RedisListenerIO for RedisListener {
    async fn subscribe(
        &self,
        channel: &str,
        payload_type: RedisPayloadType,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = Result<ConstValue, anyhow::Error>> + Send>>> {
        self.wait_for_sink(Duration::from_secs(30))
            .await
            .map_err(|e| anyhow!("Redis Pub/Sub listener not ready: {e}"))?;

        let (tx, guard) = self.register_subscriber(channel);

        {
            let mut sink_guard = self.sink.lock().await;
            let sink = sink_guard
                .as_mut()
                .ok_or_else(|| anyhow!("Redis Pub/Sub listener not connected"))?;
            sink.subscribe(channel)
                .await
                .map_err(|e| anyhow!("Failed to SUBSCRIBE to channel '{channel}': {e}"))?;
        }

        let ch = channel.to_string();
        let rx = tx.subscribe();
        let stream = stream::unfold(
            (rx, payload_type, guard),
            move |(mut rx, payload_type, guard)| {
                let ch = ch.clone();
                async move {
                    match rx.recv().await {
                        Ok(raw_payload) => {
                            let value = decode_payload(&raw_payload, &payload_type);
                            Some((Ok(value), (rx, payload_type, guard)))
                        }
                        Err(broadcast::error::RecvError::Closed) => None,
                        Err(broadcast::error::RecvError::Lagged(n)) => Some((
                            Err(anyhow!(
                                "Broadcast channel lagged (channel '{ch}'): skipped {n} messages"
                            )),
                            (rx, payload_type, guard),
                        )),
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }

    async fn read_stream(
        &self,
        key: &str,
        start_id: &str,
        payload_type: RedisPayloadType,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = Result<ConstValue, anyhow::Error>> + Send>>> {
        // `XREAD BLOCK` occupies the connection for the duration of the
        // block, so unlike Pub/Sub this cannot share a connection across
        // subscribers: each `read_stream()` call gets its own dedicated
        // connection, created lazily inside the generator below. Dropping
        // the returned stream (e.g. the GraphQL client disconnects) drops
        // `conn` at the next `.await` point, closing the connection.
        let client = self.client.clone();
        let key = key.to_string();
        let mut last_id = start_id.to_string();

        // The redis crate's default response timeout (~500ms) is far
        // shorter than `XREAD_BLOCK_MS`, so a healthy `BLOCK` cycle that
        // simply has no new data would otherwise be torn down by the
        // client itself before the server ever replies. Set an explicit
        // response timeout comfortably longer than the `BLOCK` duration.
        let conn_config = redis::AsyncConnectionConfig::new().set_response_timeout(Some(
            Duration::from_millis(XREAD_BLOCK_MS + XREAD_RESPONSE_TIMEOUT_MARGIN_MS),
        ));

        let stream = async_stream::stream! {
            let mut backoff_ms = INITIAL_RECONNECT_DELAY_MS;

            loop {
                match client.get_multiplexed_async_connection_with_config(&conn_config).await {
                    Ok(mut conn) => {
                        backoff_ms = INITIAL_RECONNECT_DELAY_MS;
                        info!(key = %key, "Redis XREAD connection established");

                        loop {
                            // `StreamReadReply::from_redis_value` treats a
                            // `BLOCK` timeout (RESP Nil) as an empty reply
                            // rather than an error, so a plain typed
                            // `query_async` handles both cases uniformly.
                            let result: redis::RedisResult<redis::streams::StreamReadReply> =
                                redis::cmd("XREAD")
                                    .arg("BLOCK")
                                    .arg(XREAD_BLOCK_MS)
                                    .arg("COUNT")
                                    .arg(XREAD_COUNT)
                                    .arg("STREAMS")
                                    .arg(&key)
                                    .arg(&last_id)
                                    .query_async(&mut conn)
                                    .await;

                            match result {
                                Ok(reply) => {
                                    for stream_key in reply.keys {
                                        for entry in stream_key.ids {
                                            last_id = entry.id.clone();
                                            match entry_to_const_value(&entry, &payload_type) {
                                                Ok(value) => yield Ok(value),
                                                Err(e) => yield Err(e),
                                            }
                                        }
                                    }
                                    // An empty reply is just a `BLOCK`
                                    // timeout; loop again without
                                    // reconnecting.
                                }
                                Err(e) if is_transient_redis_error(&e) => {
                                    // Transient (timeout/connection loss):
                                    // log and fall through to the
                                    // reconnect-with-backoff below, resuming
                                    // from `last_id` rather than surfacing
                                    // the error to the GraphQL subscriber.
                                    // This mirrors `PostgresListener`, which
                                    // never lets a dropped connection
                                    // terminate the client-facing stream.
                                    warn!(error = %e, key = %key, "XREAD failed, will reconnect from last delivered id");
                                    break;
                                }
                                Err(e) => {
                                    // Permanent failure (e.g. `WRONGTYPE`
                                    // because `key` isn't a Stream, or a
                                    // RESP type-conversion error): retrying
                                    // would silently repeat the same
                                    // failure every backoff cycle forever
                                    // while subscribers receive nothing.
                                    // Surface it and end the stream so the
                                    // misconfiguration is visible instead.
                                    error!(error = %e, key = %key, "XREAD failed with a non-transient error, ending stream");
                                    yield Err(anyhow!(
                                        "XREAD failed for key '{key}': {e}"
                                    ));
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, key = %key, "Failed to connect to Redis for XREAD, will retry");
                    }
                }

                let delay_ms = jittered_delay_ms(backoff_ms);
                backoff_ms = (backoff_ms * 2).min(MAX_RECONNECT_DELAY_MS);
                warn!(delay_ms = delay_ms, key = %key, "Reconnecting Redis XREAD in {delay_ms}ms");
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        };

        Ok(Box::pin(stream))
    }
}

/// Convert a single `XREAD` entry into the `{ id, values }` shape consumed
/// by `@redis(operation: XREAD)` subscription fields.
fn entry_to_const_value(
    entry: &StreamId,
    payload_type: &RedisPayloadType,
) -> anyhow::Result<ConstValue> {
    let mut fields = indexmap::IndexMap::with_capacity(entry.map.len());
    for (field, value) in &entry.map {
        let const_value = redis_value_to_const(value.clone())?;
        fields.insert(
            async_graphql::Name::new(field),
            decode_value_leaves(const_value, payload_type),
        );
    }

    let mut object = indexmap::IndexMap::with_capacity(2);
    object.insert(
        async_graphql::Name::new("id"),
        ConstValue::String(entry.id.clone()),
    );
    object.insert(
        async_graphql::Name::new("values"),
        ConstValue::Object(fields),
    );
    Ok(ConstValue::Object(object))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use redis::Value;

    use super::*;

    #[test]
    fn new_with_invalid_url_fails() {
        let result = RedisListener::new("not a url");
        assert!(result.is_err());
    }

    #[test]
    fn jittered_delay_stays_within_25_percent() {
        for _ in 0..100 {
            let delay = jittered_delay_ms(1000);
            assert!((750..=1250).contains(&delay), "delay out of range: {delay}");
        }
    }

    #[test]
    fn transient_redis_errors_are_classified_as_transient() {
        let connection_reset = redis::RedisError::from(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset",
        ));
        assert!(is_transient_redis_error(&connection_reset));

        let timed_out =
            redis::RedisError::from(std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout"));
        assert!(is_transient_redis_error(&timed_out));

        let connection_refused = redis::RedisError::from(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ));
        assert!(is_transient_redis_error(&connection_refused));
    }

    #[test]
    fn permanent_redis_errors_are_not_classified_as_transient() {
        // Simulates a `WRONGTYPE` server error, e.g. calling `XREAD` on a
        // key that isn't a Stream. Reconnecting will never fix this.
        let wrongtype = redis::make_extension_error(
            "WRONGTYPE".to_string(),
            Some("Operation against a key holding the wrong kind of value".to_string()),
        );
        assert!(!is_transient_redis_error(&wrongtype));

        // Simulates a RESP type-conversion error (e.g. the reply couldn't
        // be parsed into the expected `StreamReadReply` shape).
        let type_error: redis::RedisError = (
            redis::ErrorKind::UnexpectedReturnType,
            "response was of incompatible type",
        )
            .into();
        assert!(!is_transient_redis_error(&type_error));
    }

    /// Regression test for the pub/sub `senders` map growing without bound:
    /// once every receiver derived from a channel's subscription has been
    /// dropped, `SubscriptionGuard` must remove that channel's entry so a
    /// stream of disposable, argument-derived channel names doesn't leak
    /// memory (and, in the real, connected listener, server-side Pub/Sub
    /// subscriptions). This only exercises the in-process bookkeeping via
    /// `register_subscriber()`, so it needs no live Redis server.
    #[tokio::test]
    async fn subscription_guard_removes_sender_once_all_receivers_drop() {
        let listener = RedisListener::new_disconnected_for_test();

        let (tx1, guard1) = listener.register_subscriber("channel");
        let rx1 = tx1.subscribe();
        let (tx2, guard2) = listener.register_subscriber("channel");
        let rx2 = tx2.subscribe();

        // Both calls resolved to the same channel, so there is exactly one
        // entry shared by two subscribers.
        assert_eq!(listener.senders_len(), 1);

        drop(rx1);
        drop(guard1);
        // `rx2` is still alive, so the entry must not be removed yet.
        assert_eq!(
            listener.senders_len(),
            1,
            "entry removed while a receiver is still alive"
        );

        drop(rx2);
        drop(guard2);
        // No receivers remain: the last guard to drop must remove the entry.
        assert_eq!(
            listener.senders_len(),
            0,
            "entry not removed after the last receiver dropped"
        );
    }

    #[test]
    fn entry_to_const_value_builds_id_and_values() {
        let mut map = std::collections::HashMap::new();
        map.insert("name".to_string(), Value::BulkString(b"Alice".to_vec()));
        let entry = StreamId {
            id: "1-0".to_string(),
            map,
            milliseconds_elapsed_from_delivery: None,
            delivered_count: None,
        };

        let value = entry_to_const_value(&entry, &RedisPayloadType::Text).unwrap();
        match value {
            ConstValue::Object(obj) => {
                assert_eq!(obj.get("id"), Some(&ConstValue::String("1-0".to_string())));
                match obj.get("values") {
                    Some(ConstValue::Object(values)) => {
                        assert_eq!(
                            values.get("name"),
                            Some(&ConstValue::String("Alice".to_string()))
                        );
                    }
                    other => panic!("expected values object, got {other:?}"),
                }
            }
            other => panic!("expected object, got {other:?}"),
        }
    }

    #[test]
    fn entry_to_const_value_decodes_json_payload() {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "payload".to_string(),
            Value::BulkString(br#"{"n":1}"#.to_vec()),
        );
        let entry = StreamId {
            id: "2-0".to_string(),
            map,
            milliseconds_elapsed_from_delivery: None,
            delivered_count: None,
        };

        let value = entry_to_const_value(&entry, &RedisPayloadType::Json).unwrap();
        match value {
            ConstValue::Object(obj) => match obj.get("values") {
                Some(ConstValue::Object(values)) => {
                    assert!(matches!(values.get("payload"), Some(ConstValue::Object(_))));
                }
                other => panic!("expected values object, got {other:?}"),
            },
            other => panic!("expected object, got {other:?}"),
        }
    }
}
