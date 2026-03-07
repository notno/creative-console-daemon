use anyhow::{Context, Result};
use std::collections::HashMap;
use std::time::Duration;

/// HTTP client for webhook dispatch.
pub struct WebhookClient {
    client: reqwest::Client,
}

impl WebhookClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("Failed to create HTTP client");
        Self { client }
    }

    /// Send an HTTP request to the given URL.
    pub async fn send(
        &self,
        method: &str,
        url: &str,
        body: Option<&str>,
        headers: &HashMap<String, String>,
    ) -> Result<()> {
        let mut request = match method.to_uppercase().as_str() {
            "GET" => self.client.get(url),
            "POST" => self.client.post(url),
            "PUT" => self.client.put(url),
            "DELETE" => self.client.delete(url),
            _ => anyhow::bail!("Unsupported HTTP method: {method}"),
        };

        for (key, value) in headers {
            request = request.header(key.as_str(), value.as_str());
        }

        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }

        let response = request.send().await.context("Webhook request failed")?;
        let status = response.status();

        if status.is_success() {
            tracing::info!(url, status = %status, "Webhook sent successfully");
        } else {
            tracing::warn!(url, status = %status, "Webhook returned non-success status");
        }

        Ok(())
    }
}
