use std::sync::Arc;

use async_trait::async_trait;

use crate::Result;
use crate::config::AppConfig;
use crate::slack::SlackClient;

use super::{Sink, SinkMessage, SinkTarget};

#[derive(Clone)]
pub struct SlackSink {
    client: SlackClient,
}

impl SlackSink {
    pub fn new(client: SlackClient) -> Self {
        Self { client }
    }

    pub fn from_config(config: Arc<AppConfig>) -> Result<Self> {
        Ok(Self::new(SlackClient::from_config(config)?))
    }
}

impl Default for SlackSink {
    fn default() -> Self {
        Self::new(SlackClient::new())
    }
}

#[async_trait]
impl Sink for SlackSink {
    async fn send(&self, target: &SinkTarget, message: &SinkMessage) -> Result<()> {
        self.client.send(target, message).await
    }
}
