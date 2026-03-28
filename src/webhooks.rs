use std::collections::HashSet;
use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub event: String,
    pub project: String,
    pub data: serde_json::Value,
    pub timestamp: String,
}

pub fn fire(
    url: &str,
    events: &HashSet<String>,
    event: &str,
    project: &str,
    data: serde_json::Value,
) {
    if !events.contains(event) {
        return;
    }
    let payload = WebhookPayload {
        event: event.to_string(),
        project: project.to_string(),
        data,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    let url = url.to_string();
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        if let Err(e) = client.post(&url).json(&payload).send().await {
            tracing::warn!(event = payload.event, error = %e, "Webhook delivery failed");
        } else {
            tracing::debug!(event = payload.event, "Webhook delivered");
        }
    });
}
