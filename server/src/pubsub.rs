use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info};
use uuid::Uuid;

use crate::models::PubSubClipMessage;

pub struct PubSubRelay {
    channels: Arc<RwLock<HashMap<Uuid, broadcast::Sender<PubSubClipMessage>>>>,
    redis_url: Option<String>,
}

impl PubSubRelay {
    pub fn new(redis_url: Option<String>) -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
            redis_url,
        }
    }

    pub async fn subscribe(&self, user_id: Uuid) -> broadcast::Receiver<PubSubClipMessage> {
        let mut channels = self.channels.write().await;
        let sender = channels
            .entry(user_id)
            .or_insert_with(|| broadcast::channel(256).0);
        sender.subscribe()
    }

    pub async fn publish(&self, msg: PubSubClipMessage) -> anyhow::Result<()> {
        // Publish to local broadcast channels
        let channels = self.channels.read().await;
        if let Some(sender) = channels.get(&msg.user_id) {
            let _ = sender.send(msg.clone());
        }
        drop(channels);

        // Publish to Redis if configured
        if let Some(ref url) = self.redis_url {
            self.publish_redis(url, &msg).await?;
        }

        Ok(())
    }

    async fn publish_redis(&self, url: &str, msg: &PubSubClipMessage) -> anyhow::Result<()> {
        let client = redis::Client::open(url)?;
        let mut conn = client.get_multiplexed_async_connection().await?;
        let channel = format!("clips:user:{}", msg.user_id);
        let payload = serde_json::to_string(msg)?;
        redis::cmd("PUBLISH")
            .arg(&channel)
            .arg(&payload)
            .query_async::<()>(&mut conn)
            .await?;
        info!(channel = %channel, "Published clip to Redis");
        Ok(())
    }

    pub async fn start_redis_subscriber(self: Arc<Self>) -> anyhow::Result<()> {
        let url = match &self.redis_url {
            Some(url) => url.clone(),
            None => return Ok(()),
        };

        tokio::spawn(async move {
            loop {
                match Self::redis_subscribe_loop(&url, &self).await {
                    Ok(()) => info!("Redis subscriber disconnected, reconnecting..."),
                    Err(e) => error!("Redis subscriber error: {e}, reconnecting in 5s..."),
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });

        Ok(())
    }

    async fn redis_subscribe_loop(url: &str, relay: &Arc<Self>) -> anyhow::Result<()> {
        let client = redis::Client::open(url)?;
        let mut pubsub = client.get_async_pubsub().await?;
        pubsub.psubscribe("clips:user:*").await?;

        info!("Redis subscriber connected, listening on clips:user:*");

        let mut stream = pubsub.on_message();
        use futures_util::StreamExt;
        while let Some(msg) = stream.next().await {
            let payload: String = msg.get_payload()?;
            if let Ok(clip_msg) = serde_json::from_str::<PubSubClipMessage>(&payload) {
                let channels = relay.channels.read().await;
                if let Some(sender) = channels.get(&clip_msg.user_id) {
                    let _ = sender.send(clip_msg);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn test_local_pubsub() {
        let relay = PubSubRelay::new(None);
        let user_id = Uuid::new_v4();

        let mut rx = relay.subscribe(user_id).await;

        let msg = PubSubClipMessage {
            msg_type: "clip".into(),
            user_id,
            clip_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            content: "test content".into(),
            e2e: false,
        };

        relay.publish(msg.clone()).await.unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.content, "test content");
    }
}
