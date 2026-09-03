//! AI-assisted permission classification for jcode.
//!
//! This crate implements the classifier for Auto Mode, a feature that uses AI to
//! evaluate permission requests. The classifier determines whether a tool call is
//! authorized by the user's request and aligned with safe practices.
//!
//! # Architecture
//!
//! The classifier operates in two stages:
//!
//! 1. **Stage 1 (Fast Filter)**: A lightweight evaluation that returns YES/NO
//! 2. **Stage 2 (Full Reasoning)**: A thorough evaluation when Stage 1 flags an action
//!
//! Both stages return REASON + DECISION for transparency and debugging.
//!
//! # Design Principles
//!
//! - **Reuses existing Provider infrastructure**: No new auth/API keys needed
//! - **Reasoned decisions**: Both stages include reasoning for transparency
//! - **Configurable**: Trust boundaries, block rules, and allow exceptions
//! - **Deny-and-continue**: Blocked actions prompt agent to try alternative path

pub mod config;
pub mod decision;
pub mod denial_tracker;
pub mod input;
pub mod stages;

// Re-export types
pub use config::{BlockRules, ClassifierConfig, TrustBoundaries};
pub use decision::{BlockCategory, Decision};
pub use denial_tracker::DenialTracker;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use jcode_message_types::{ContentBlock, Message, Role};

pub use input::TurnContext;

/// Main classifier trait.
///
/// Implementors provide the actual AI classification logic. The session provider
/// classifier uses the existing Provider infrastructure.
#[async_trait]
pub trait Classifier: Send + Sync {
    /// Evaluate a permission request.
    async fn classify(&self, input: &ClassifierInput) -> Result<ClassifierResult>;

    /// Name of the model being used (for display/debugging).
    fn model_name(&self) -> String;
}

/// Input to the classifier.
#[derive(Debug, Clone)]
pub struct ClassifierInput {
    /// The user's actual request (what they asked for).
    pub user_message: String,

    /// The tool call being requested.
    pub tool_call: ToolCall,

    /// Recent conversation history (truncated).
    pub recent_history: Vec<Message>,

    /// Working directory.
    pub working_directory: PathBuf,

    /// Classifier configuration.
    pub config: config::ClassifierConfig,

    /// Full turn context: the user's request for the current turn plus the
    /// surrounding conversation. When available this is preferred over the
    /// individual `user_message` / `recent_history` fields because it carries
    /// the exact text the agent is acting on.
    pub turn_context: Option<TurnContext>,
}

/// A tool call to be evaluated.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Tool name (e.g., "bash", "write", "browser").
    pub name: String,

    /// Tool arguments as JSON.
    pub arguments: serde_json::Value,
}

/// Output from the classifier.
#[derive(Debug, Clone)]
pub struct ClassifierResult {
    /// The decision.
    pub decision: decision::Decision,

    /// Human-readable reasoning.
    pub reason: String,

    /// Which stage produced this result (1 or 2).
    pub stage: u8,
}

/// Classifier implementation using the session's current Provider.
///
/// This reuses all existing provider configuration:
/// - Auth (OAuth, API keys, etc.)
/// - Model selection
/// - API routing
/// - Rate limiting, retries, etc.
pub struct SessionProviderClassifier {
    provider: Arc<dyn jcode_provider_core::Provider>,
    config: config::ClassifierConfig,
}

impl SessionProviderClassifier {
    /// Create a new classifier using the session's provider.
    pub fn new(provider: Arc<dyn jcode_provider_core::Provider>, config: config::ClassifierConfig) -> Self {
        Self { provider, config }
    }
}

#[async_trait]
impl Classifier for SessionProviderClassifier {
    async fn classify(&self, input: &ClassifierInput) -> Result<ClassifierResult> {
        // Stage 1: fast filter. Returns YES/NO. A YES is a confident allow;
        // a NO is a flag for deeper review rather than an immediate deny, so we
        // escalate to Stage 2 for the full reasoning pass.
        let stage1 = self.run_stage(&stages::build_stage1, stages::parse_stage1_response, input, 1).await?;

        match stage1.decision {
            decision::Decision::Allow => Ok(stage1),
            decision::Decision::Block { .. } => {
                // Stage 2: thorough evaluation. This re-runs with a more careful
                // prompt and history context, and is the final word. Network or
                // parse failures here fall back to the Stage 1 verdict so a
                // transient issue does not silently approve a flagged action.
                match self
                    .run_stage(&stages::build_stage2, stages::parse_stage2_response, input, 2)
                    .await
                {
                    Ok(stage2) => Ok(stage2),
                    Err(e) => {
                        // Preserve the Stage 1 denial but annotate that Stage 2
                        // could not run, so callers can see why the full review
                        // was skipped.
                        let mut result = stage1;
                        if !result.reason.contains("Stage 2 unavailable") {
                            result.reason = format!("{} (Stage 2 unavailable: {})", result.reason, e);
                        }
                        Ok(result)
                    }
                }
            }
        }
    }

    fn model_name(&self) -> String {
        self.provider.model()
    }
}

impl SessionProviderClassifier {
    /// Run a single classification stage: build the prompt, call the provider,
    /// collect the response, and parse it.
    async fn run_stage(
        &self,
        build: impl Fn(&ClassifierInput) -> (String, String),
        parse: impl Fn(&str, u8) -> Result<ClassifierResult>,
        input: &ClassifierInput,
        stage: u8,
    ) -> Result<ClassifierResult> {
        let (system_prompt, user_message) = build(input);

        let content = vec![ContentBlock::Text {
            text: user_message,
            cache_control: None,
        }];
        let messages = vec![Message {
            role: Role::User,
            content,
            timestamp: None,
            tool_duration_ms: None,
        }];

        let stream = self
            .provider
            .complete(&messages, &[], &system_prompt, None)
            .await?;

        let response = collect_response(stream).await?;
        parse(&response, stage)
    }
}

async fn collect_response(
    stream: jcode_provider_core::EventStream,
) -> Result<String> {
    use futures_util::StreamExt;
    use jcode_message_types::StreamEvent;

    let mut stream = std::pin::pin!(stream);
    let mut response = String::new();

    while let Some(event_result) = stream.next().await {
        let event = event_result?;
        match event {
            StreamEvent::TextDelta(text) => {
                response.push_str(&text);
            }
            StreamEvent::MessageEnd { .. } => {
                break;
            }
            _ => {
                // Ignore other events
            }
        }
    }

    Ok(response)
}
