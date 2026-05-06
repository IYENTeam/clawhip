pub mod discord;
pub mod hermes;
pub mod iyensystem;
pub mod openclaw;
pub mod slack;

use async_trait::async_trait;

use crate::Result;
use crate::events::MessageFormat;
use serde_json::Value;

pub use discord::DiscordSink;
pub use hermes::HermesSink;
pub use iyensystem::IyenSystemSink;
pub use openclaw::OpenClawSink;
pub use slack::SlackSink;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SinkTarget {
    DiscordChannel(String),
    DiscordWebhook(String),
    SlackWebhook(String),
    OpenClaw,
    IyenSystem,
    /// Hermes Agent gateway — peer to OpenClaw as a decision authority
    /// for IYEN label-driven lanes. Carries no per-target data because
    /// the gateway URL/token live in `[providers.hermes]` config.
    Hermes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkMessage {
    pub event_kind: String,
    pub format: MessageFormat,
    pub content: String,
    pub payload: Value,
}

#[async_trait]
pub trait Sink: Send + Sync {
    async fn send(&self, target: &SinkTarget, message: &SinkMessage) -> Result<()>;
}
