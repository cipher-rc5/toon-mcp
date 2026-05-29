//! Thin blocking client for a local llama.cpp `llama-server`.
//!
//! Uses two endpoints:
//! - `POST /v1/chat/completions` (OpenAI-compatible) for generation + judging.
//! - `POST /tokenize` for an *exact* token count under the loaded model's
//!   tokenizer — this is the figure that actually matters for context budget,
//!   so we prefer it over any portable BPE approximation.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;
use ureq::Agent;

pub struct LlmClient {
    agent: Agent,
    base_url: String,
    model: String,
}

impl LlmClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        // Generation on a local GGUF can be slow; give it generous timeouts.
        let config = Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(600)))
            .build();
        Self {
            agent: Agent::new_with_config(config),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
        }
    }

    /// Sanity-check that the server is reachable.
    pub fn ping(&self) -> Result<()> {
        let url = format!("{}/v1/models", self.base_url);
        self.agent
            .get(&url)
            .call()
            .with_context(|| format!("llama-server not reachable at {}", self.base_url))?;
        Ok(())
    }

    /// One chat completion. Returns the assistant message content (trimmed).
    pub fn chat(
        &self,
        system: &str,
        user: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });
        let resp: Value = self
            .agent
            .post(&url)
            .send_json(&body)
            .context("chat request failed")?
            .into_body()
            .read_json()
            .context("chat response was not JSON")?;

        let content = resp
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::trim)
            .map(str::to_owned);
        match content {
            Some(c) if !c.is_empty() => Ok(c),
            _ => bail!("empty/malformed chat response: {resp}"),
        }
    }

    /// Exact token count for `text` under the server's loaded tokenizer.
    pub fn count_tokens(&self, text: &str) -> Result<usize> {
        let url = format!("{}/tokenize", self.base_url);
        let resp: Value = self
            .agent
            .post(&url)
            .send_json(json!({ "content": text }))
            .context("tokenize request failed")?
            .into_body()
            .read_json()
            .context("tokenize response was not JSON")?;
        let tokens = resp
            .get("tokens")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .context("tokenize response missing `tokens` array")?;
        Ok(tokens)
    }
}
