use crate::Event;

pub mod ga;
pub mod posthog;

///
/// Defines the interface for an event collector.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait Collect: Send + Sync {
    async fn collect(&self, event: Event) -> super::Result<()>;
}
