//! `open-runo-ai-routing`: chooses the best AI provider/model for a request
//! based on a configurable [`RoutingPolicy`], per the README's AI Routing
//! Engine section.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use open_runo_core::{AppError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    OpenAi,
    AnthropicClaude,
    GoogleGemini,
    DeepSeek,
    LocalLlm,
    CustomOpenAiCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingPolicy {
    CostOptimized,
    LatencyOptimized,
    LocalFirst,
    PrivacyFirst,
}

/// A candidate provider along with the metrics the router needs to rank it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub provider: Provider,
    pub estimated_cost_usd_per_1k_tokens: f64,
    pub estimated_latency_ms: u32,
    pub is_local: bool,
    pub context_length: u32,
}

/// Routes a request to the best candidate for the given policy and minimum
/// required context length. Returns [`AppError::NotFound`] if nothing
/// qualifies.
pub fn route(
    candidates: &[Candidate],
    policy: RoutingPolicy,
    min_context_length: u32,
) -> Result<&Candidate> {
    let eligible: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.context_length >= min_context_length)
        .collect();

    if eligible.is_empty() {
        return Err(AppError::NotFound(format!(
            "no provider satisfies min_context_length={min_context_length}"
        )));
    }

    let chosen = match policy {
        RoutingPolicy::CostOptimized => eligible.into_iter().min_by(|a, b| {
            a.estimated_cost_usd_per_1k_tokens
                .total_cmp(&b.estimated_cost_usd_per_1k_tokens)
        }),
        RoutingPolicy::LatencyOptimized => {
            eligible.into_iter().min_by_key(|c| c.estimated_latency_ms)
        },
        RoutingPolicy::LocalFirst | RoutingPolicy::PrivacyFirst => eligible
            .iter()
            .find(|c| c.is_local)
            .copied()
            .or_else(|| eligible.into_iter().min_by_key(|c| c.estimated_latency_ms)),
    };

    // `eligible` was checked non-empty above, so every match arm above
    // yields `Some`; we still surface a proper error instead of
    // panicking in case that invariant is ever broken by a future edit.
    chosen
        .ok_or_else(|| AppError::Internal("routing candidate selection produced no result".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> Vec<Candidate> {
        vec![
            Candidate {
                provider: Provider::LocalLlm,
                estimated_cost_usd_per_1k_tokens: 0.0,
                estimated_latency_ms: 800,
                is_local: true,
                context_length: 8_000,
            },
            Candidate {
                provider: Provider::AnthropicClaude,
                estimated_cost_usd_per_1k_tokens: 3.0,
                estimated_latency_ms: 400,
                is_local: false,
                context_length: 200_000,
            },
        ]
    }

    #[test]
    fn cost_optimized_prefers_free_local_model() {
        let cands = candidates();
        let chosen = route(&cands, RoutingPolicy::CostOptimized, 4_000).unwrap();
        assert_eq!(chosen.provider, Provider::LocalLlm);
    }

    #[test]
    fn latency_optimized_prefers_faster_cloud_model() {
        let cands = candidates();
        let chosen = route(&cands, RoutingPolicy::LatencyOptimized, 4_000).unwrap();
        assert_eq!(chosen.provider, Provider::AnthropicClaude);
    }

    #[test]
    fn privacy_first_prefers_local_when_context_fits() {
        let cands = candidates();
        let chosen = route(&cands, RoutingPolicy::PrivacyFirst, 4_000).unwrap();
        assert_eq!(chosen.provider, Provider::LocalLlm);
    }

    #[test]
    fn errors_when_context_length_unmet() {
        let cands = candidates();
        let err = route(&cands, RoutingPolicy::CostOptimized, 1_000_000).unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }
}
