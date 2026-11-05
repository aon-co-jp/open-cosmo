//! Picks an AI provider under three different routing policies for the
//! same candidate pool, illustrating how `RoutingPolicy` changes the
//! outcome for an identical request.
//!
//! Run with:
//!   cargo run -p open-runo-ai-routing --example pick_provider

use open_runo_ai_routing::{route, Candidate, Provider, RoutingPolicy};

fn candidates() -> Vec<Candidate> {
    vec![
        Candidate {
            provider: Provider::LocalLlm,
            estimated_cost_usd_per_1k_tokens: 0.0,
            estimated_latency_ms: 900,
            is_local: true,
            context_length: 8_000,
        },
        Candidate {
            provider: Provider::AnthropicClaude,
            estimated_cost_usd_per_1k_tokens: 3.0,
            estimated_latency_ms: 350,
            is_local: false,
            context_length: 200_000,
        },
        Candidate {
            provider: Provider::DeepSeek,
            estimated_cost_usd_per_1k_tokens: 0.5,
            estimated_latency_ms: 600,
            is_local: false,
            context_length: 64_000,
        },
    ]
}

fn main() -> open_runo_core::Result<()> {
    let pool = candidates();
    let min_context_length = 4_000;

    for policy in [
        RoutingPolicy::CostOptimized,
        RoutingPolicy::LatencyOptimized,
        RoutingPolicy::PrivacyFirst,
    ] {
        let chosen = route(&pool, policy, min_context_length)?;
        println!(
            "{policy:?} -> {:?} (cost=${:.2}/1k tok, latency={}ms)",
            chosen.provider, chosen.estimated_cost_usd_per_1k_tokens, chosen.estimated_latency_ms
        );
    }

    // A request needing more context than the local model supports still
    // routes correctly even under a privacy-first policy.
    let chosen = route(&pool, RoutingPolicy::PrivacyFirst, 100_000)?;
    println!(
        "PrivacyFirst with min_context_length=100_000 -> {:?} (local model doesn't qualify)",
        chosen.provider
    );

    Ok(())
}
