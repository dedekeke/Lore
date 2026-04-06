use std::collections::HashSet;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub event: String,
    pub project: String,
    pub data: serde_json::Value,
    pub timestamp: String,
}

pub fn fire(
    client: &reqwest::Client,
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
    let client = client.clone();
    tokio::spawn(async move {
        if let Err(e) = client.post(&url).json(&payload).send().await {
            tracing::warn!(event = payload.event, error = %e, "Webhook delivery failed");
        } else {
            tracing::debug!(event = payload.event, "Webhook delivered");
        }
    });
}
