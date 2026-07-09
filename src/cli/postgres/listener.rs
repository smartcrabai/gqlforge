use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use futures_util::stream;
use gqlrs_value::ConstValue;
use tokio::sync::broadcast;
use tokio_postgres::AsyncMessage;
use tracing::{debug, error, info, trace, warn};

use crate::core::config::PostgresPayloadType;
use crate::core::postgres::{PostgresListenerIO, quote_ident};

const BROADCAST_CAPACITY: usize = 256;
const INITIAL_RECONNECT_DELAY_MS: u64 = 1000;
const MAX_RECONNECT_DELAY_MS: u64 = 30000;
const CLIENT_INIT_POLL_INTERVAL_MS: u64 = 100;

pub struct PostgresListener {
    senders: DashMap<String, broadcast::Sender<String>>,
    client: tokio::sync::Mutex<Option<tokio_postgres::Client>>,
}

impl PostgresListener {
    pub fn new(connection_url: &str) -> Arc<Self> {
        let listener = Arc::new(Self {
            senders: DashMap::new(),
            client: tokio::sync::Mutex::new(None),
        });

        let driver_listener = listener.clone();
        let driver_url = connection_url.to_string();

        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    driver_listener.run_driver(driver_url).await;
                });
            }
            Err(_) => {
                warn!(
                    "No Tokio runtime available at PostgresListener construction; listener will not function without a running Tokio runtime"
                );
            }
        }

        listener
    }

    async fn run_driver(self: Arc<Self>, url: String) {
        let mut backoff_ms = INITIAL_RECONNECT_DELAY_MS;

        loop {
            match crate::core::postgres::make_tls_connect() {
                Ok(tls) => match tokio_postgres::connect(url.as_str(), tls).await {
                    Ok((new_client, connection)) => {
                        {
                            let mut client_guard = self.client.lock().await;
                            *client_guard = Some(new_client);
                        }

                        backoff_ms = INITIAL_RECONNECT_DELAY_MS;
                        self.reissue_listen_all().await;

                        let mut conn = connection;

                        info!("PostgreSQL listener connection established");

                        loop {
                            match futures_util::future::poll_fn(|cx| conn.poll_message(cx)).await {
                                Some(Ok(AsyncMessage::Notification(notification))) => {
                                    let channel = notification.channel().to_string();
                                    let payload = notification.payload().to_string();
                                    self.send_notification(&channel, payload);
                                }
                                Some(Ok(AsyncMessage::Notice(notice))) => {
                                    debug!(
                                        severity = %notice.severity(),
                                        message = %notice.message(),
                                        "PostgreSQL notice"
                                    );
                                }
                                Some(Ok(_)) => {
                                    trace!("PostgreSQL async message");
                                }
                                Some(Err(e)) => {
                                    error!(
                                        error = %e,
                                        "PostgreSQL connection lost, will reconnect"
                                    );
                                    break;
                                }
                                None => {
                                    info!("PostgreSQL connection closed, will reconnect");
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(
                            error = %e,
                            "Failed to connect to PostgreSQL listener"
                        );
                    }
                },
                Err(e) => {
                    error!(
                        error = %e,
                        "Failed to create TLS config for PostgreSQL listener"
                    );
                }
            }

            backoff_ms = (backoff_ms * 2).min(MAX_RECONNECT_DELAY_MS);
            // Add +/-25% jitter to prevent thundering herd when multiple listeners
            // reconnect
            let jitter: u64 = rand::random_range(0..=backoff_ms / 4);
            let delay_ms = if rand::random_bool(0.5) {
                backoff_ms.saturating_sub(jitter)
            } else {
                backoff_ms.saturating_add(jitter)
            };
            warn!(
                delay_ms = delay_ms,
                "Reconnecting PostgreSQL listener in {delay_ms}ms"
            );
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    async fn wait_for_client(&self, timeout: Duration) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.client.lock().await.is_some() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("Timeout waiting for PostgreSQL listener client to initialize");
            }
            tokio::time::sleep(Duration::from_millis(CLIENT_INIT_POLL_INTERVAL_MS)).await;
        }
    }

    async fn reissue_listen_all(&self) {
        let client_guard = self.client.lock().await;
        let Some(client) = client_guard.as_ref() else {
            return;
        };
        for entry in &self.senders {
            let query = format!("LISTEN {}", quote_ident(entry.key()));
            if let Err(e) = client.batch_execute(&query).await {
                error!(
                    error = %e,
                    channel = %entry.key(),
                    "Failed to re-issue LISTEN after reconnection"
                );
            }
        }
    }

    fn send_notification(&self, channel: &str, payload: String) {
        if let Some(tx) = self.senders.get(channel) {
            match tx.send(payload) {
                Ok(n) => {
                    trace!(
                        channel = %channel,
                        receivers = n,
                        "Sent notification to broadcast channel"
                    );
                }
                Err(_send_err) => {
                    trace!(
                        channel = %channel,
                        "Notification dropped: no receivers for channel"
                    );
                }
            }
        } else {
            trace!(
                channel = %channel,
                "Notification for untracked channel, ignoring"
            );
        }
    }
}

#[async_trait::async_trait]
impl PostgresListenerIO for PostgresListener {
    async fn subscribe(
        &self,
        channel: &str,
        payload_type: PostgresPayloadType,
    ) -> anyhow::Result<
        Pin<Box<dyn futures_util::Stream<Item = Result<ConstValue, anyhow::Error>> + Send>>,
    > {
        self.wait_for_client(Duration::from_secs(30))
            .await
            .map_err(|e| anyhow!("PostgreSQL listener not ready: {e}"))?;

        let tx = match self.senders.entry(channel.to_string()) {
            Entry::Occupied(e) => e.get().clone(),
            Entry::Vacant(e) => {
                let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
                e.insert(tx.clone());
                tx
            }
        };

        let query = format!("LISTEN {}", quote_ident(channel));
        let client_guard = self.client.lock().await;
        let client = client_guard
            .as_ref()
            .ok_or_else(|| anyhow!("PostgreSQL listener not connected"))?;
        client
            .batch_execute(&query)
            .await
            .map_err(|e| anyhow!("Failed to execute LISTEN on channel '{channel}': {e}"))?;
        drop(client_guard);

        let ch = channel.to_string();
        let rx = tx.subscribe();
        let stream = stream::unfold((rx, payload_type), move |(mut rx, payload_type)| {
            let ch = ch.clone();
            async move {
                match rx.recv().await {
                    Ok(raw_payload) => {
                        let value = match payload_type {
                            PostgresPayloadType::Json => match serde_json::from_str(&raw_payload) {
                                Ok(v) => v,
                                Err(e) => {
                                    return Some((
                                        Err(anyhow!(
                                            "Invalid NOTIFY JSON payload (channel '{ch}'): {e}"
                                        )),
                                        (rx, payload_type),
                                    ));
                                }
                            },
                            PostgresPayloadType::Text => ConstValue::String(raw_payload),
                        };
                        Some((Ok(value), (rx, payload_type)))
                    }
                    Err(broadcast::error::RecvError::Closed) => None,
                    Err(broadcast::error::RecvError::Lagged(n)) => Some((
                        Err(anyhow!(
                            "Broadcast channel lagged (channel '{ch}'): skipped {n} messages"
                        )),
                        (rx, payload_type),
                    )),
                }
            }
        });

        Ok(Box::pin(stream))
    }
}
