use std::collections::HashSet;
use std::process::Output;

use chrono::{DateTime, Utc};
use machineid_rs::{Encryption, HWIDComponent, IdBuilder};
use sysinfo::System;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::Duration;

use super::Result;
use crate::can_track::can_track;
use crate::collect::{Collect, ga, posthog};
use crate::{Event, EventKind};

const GA_API_SECRET: &str = match option_env!("GA_API_SECRET") {
    Some(val) => val,
    None => "dev",
};
const GA_MEASUREMENT_ID: &str = match option_env!("GA_MEASUREMENT_ID") {
    Some(val) => val,
    None => "dev",
};
const POSTHOG_API_SECRET: &str = match option_env!("POSTHOG_API_SECRET") {
    Some(val) => val,
    None => "dev",
};

const PARAPHRASE: &str = "gqlforge_key";

const DEFAULT_CLIENT_ID: &str = "<anonymous>";

pub struct Tracker {
    collectors: Vec<Box<dyn Collect>>,
    can_track: bool,
    start_time: DateTime<Utc>,
    email: Mutex<Option<Vec<String>>>,
}

impl Default for Tracker {
    fn default() -> Self {
        let ga_tracker = Box::new(ga::Tracker::new(
            GA_API_SECRET.to_string(),
            GA_MEASUREMENT_ID.to_string(),
        ));
        let posthog_tracker = Box::new(posthog::Tracker::new(POSTHOG_API_SECRET));
        let start_time = Utc::now();
        let can_track = can_track();
        Self {
            collectors: vec![ga_tracker, posthog_tracker],
            can_track,
            start_time,
            email: Mutex::new(None),
        }
    }
}

impl Tracker {
    pub fn init_ping(&'static self, duration: Duration) {
        let mut interval = tokio::time::interval(duration);
        tokio::task::spawn(async move {
            loop {
                interval.tick().await;
                let _ = self.dispatch(EventKind::Ping).await;
            }
        });
    }

    /// # Errors
    ///
    /// Returns an error if event collection fails.
    pub async fn dispatch(&'static self, event_kind: EventKind) -> Result<()> {
        if self.can_track {
            // Create a new event
            let event = Event {
                event_name: event_kind.name(),
                start_time: self.start_time,
                cores: cores(),
                client_id: client_id(),
                os_name: os_name(),
                up_time: up_time(self.start_time),
                args: args(),
                path: path(),
                cwd: cwd(),
                user: user(),
                version: version(),
                email: self.email().await.clone(),
            };

            // Dispatch the event to all collectors
            for collector in &self.collectors {
                collector.collect(event.clone()).await?;
            }

            tracing::debug!("Event dispatched: {:?}", event);
        }

        Ok(())
    }

    async fn email(&'static self) -> Vec<String> {
        let mut guard = self.email.lock().await;
        if guard.is_none() {
            *guard = Some(email().await.into_iter().collect());
        }
        guard.clone().unwrap_or_default()
    }
}

// Get the email address
async fn email() -> HashSet<String> {
    fn parse(output: &Output) -> Option<String> {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }

        None
    }

    // From Git
    async fn git() -> Result<Output> {
        Ok(Command::new("git")
            .args(["config", "--global", "user.email"])
            .output()
            .await?)
    }

    // From SSH Keys
    async fn ssh() -> Result<Output> {
        Ok(Command::new("sh")
            .args(["-c", "cat ~/.ssh/*.pub"])
            .output()
            .await?)
    }

    // From defaults read MobileMeAccounts Accounts
    async fn mobile_me() -> Result<Output> {
        Ok(Command::new("defaults")
            .args(["read", "MobileMeAccounts", "Accounts"])
            .output()
            .await?)
    }

    vec![git().await, ssh().await, mobile_me().await]
        .into_iter()
        .flat_map(|output| {
            output
                .ok()
                .as_ref()
                .and_then(parse)
                .map_or_default(|s| parse_email(&s))
        })
        .collect::<HashSet<String>>()
}

// Generates a random client ID
fn client_id() -> String {
    let mut builder = IdBuilder::new(Encryption::SHA256);
    builder
        .add_component(HWIDComponent::SystemID)
        .add_component(HWIDComponent::CPUCores);
    builder
        .build(PARAPHRASE)
        .unwrap_or(DEFAULT_CLIENT_ID.to_string())
}

// Get the number of CPU cores
fn cores() -> usize {
    System::physical_core_count().unwrap_or(0)
}

// Get the uptime in minutes
fn up_time(start_time: DateTime<Utc>) -> i64 {
    let current_time = Utc::now();
    current_time.signed_duration_since(start_time).num_minutes()
}

fn version() -> String {
    gqlforge_version::VERSION.as_str().to_string()
}

fn user() -> String {
    whoami::username().unwrap_or_default()
}

fn cwd() -> Option<String> {
    std::env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(std::string::ToString::to_string))
}

fn path() -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(std::string::ToString::to_string))
}

fn args() -> Vec<String> {
    std::env::args().skip(1).collect()
}

fn os_name() -> String {
    System::long_os_version().unwrap_or("Unknown".to_string())
}

// Should take arbitrary text and be able to extract email addresses
fn parse_email(text: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    let Some(re) = RE
        .get_or_init(|| regex::Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").ok())
        .as_ref()
    else {
        return vec![];
    };
    re.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use super::*;

    static TRACKER: LazyLock<Tracker> = LazyLock::new(Tracker::default);

    #[tokio::test]
    async fn test_tracker() {
        if let Err(e) = TRACKER
            .dispatch(EventKind::Command("ping".to_string()))
            .await
        {
            panic!("Tracker dispatch error: {e:?}");
        }
    }
}
