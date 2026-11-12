//! The tokens a request used, as the engine reports them.

use serde_json::Value;

/// One model's use in a request. Claude Code reports each model once at
/// the end (with the models it uses on its own); pi reports each answer,
/// and the pi-censor extension each classifier call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Call {
    /// "model": the requested model (or one the engine used for it);
    /// "classifier": pi-censor judging a tool call.
    pub role: &'static str,
    pub provider: String,
    pub model: String,
    /// Tokens read anew, not from the cache.
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Of `cache_write`, the part kept for an hour (Claude).
    pub cache_write_1h: Option<u64>,
    pub output: u64,
    /// Of `output`, the thinking, where the engine counts it apart.
    pub thinking: Option<u64>,
    pub web_searches: Option<u64>,
    /// In US dollars, as the engine prices it (Claude: list price).
    pub cost: Option<f64>,
    pub ms: Option<u64>,
}

impl Call {
    /// True if the call used no tokens. A failed answer often reports
    /// such a call, and it says nothing about the usage.
    pub fn is_empty(&self) -> bool {
        self.input + self.cache_read + self.cache_write + self.output == 0
    }

    /// One model of Claude Code's `modelUsage` (in its `result` event).
    pub(crate) fn claude(model: &str, usage: &Value) -> Call {
        Call {
            role: "model",
            provider: "anthropic".into(),
            model: model.into(),
            input: number(&usage["inputTokens"]),
            cache_read: number(&usage["cacheReadInputTokens"]),
            cache_write: number(&usage["cacheCreationInputTokens"]),
            cache_write_1h: usage["cacheCreationEphemeral1hInputTokens"].as_u64(),
            output: number(&usage["outputTokens"]),
            thinking: usage["thinkingTokens"].as_u64(),
            web_searches: usage["webSearchRequests"].as_u64(),
            cost: usage["costUSD"].as_f64(),
            ms: None,
        }
    }

    /// A call from pi's usage (`Usage` in pi's message types).
    pub(crate) fn pi(
        role: &'static str,
        provider: &Value,
        model: &Value,
        usage: &Value,
        ms: &Value,
    ) -> Call {
        // pi writes 0 for thinking it cannot count apart: that is unknown.
        let thinking = usage["reasoning"].as_u64().filter(|&n| n > 0);
        Call {
            role,
            provider: provider.as_str().unwrap_or_default().into(),
            model: model.as_str().unwrap_or_default().into(),
            input: number(&usage["input"]),
            cache_read: number(&usage["cacheRead"]),
            cache_write: number(&usage["cacheWrite"]),
            cache_write_1h: usage["cacheWrite1h"].as_u64(),
            output: number(&usage["output"]),
            thinking,
            web_searches: None,
            cost: usage["cost"]["total"].as_f64(),
            ms: ms.as_u64(),
        }
    }
}

pub(crate) fn number(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_call() {
        assert!(Call::default().is_empty());
        assert!(!Call {
            cache_read: 1,
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn pi_thinking_zero_is_unknown() {
        let usage = json!({ "input": 5, "output": 2, "reasoning": 0, "cost": { "total": 0.5 } });
        let call = Call::pi(
            "model",
            &json!("ollama"),
            &json!("qwen"),
            &usage,
            &json!(30),
        );
        assert_eq!(call.thinking, None);
        assert_eq!(
            (call.input, call.output, call.cost, call.ms),
            (5, 2, Some(0.5), Some(30))
        );
    }
}
