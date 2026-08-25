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

// Re-export config types and decision types
pub use config::{AutoModeConfig, BlockRules, ClassifierConfig, PermissionMode, PermissionsConfig, Stage1Config, TrustBoundaries};
pub use decision::{BlockCategory, Decision};
pub use denial_tracker::DenialTracker;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use jcode_message_types::{ContentBlock, Message, Role};

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
        // Build Stage 1 prompt
        let (system_prompt, user_message) = stages::build_stage1(&input);

        // Create a single user message with ContentBlock
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

        // Call the provider
        let stream = self.provider.complete(
            &messages,
            &[], // No tools needed for classification
            &system_prompt,
            None,
        ).await?;

        // Collect the response
        let response = collect_response(stream).await?;

        // Parse the response
        stages::parse_stage1_response(&response, 1)
    }

    fn model_name(&self) -> String {
        self.provider.model()
    }
}

impl config::ClassifierConfig {
    /// Create a default configuration.
    pub fn default_config() -> Self {
        Self {
            model_route: None,
            trust_boundaries: config::TrustBoundaries::default(),
            block_rules: config::BlockRules::default(),
            allow_exceptions: vec![
                "package_install_from_lockfile".to_string(),
                "standard_credential_flows".to_string(),
                "push_to_working_branch".to_string(),
            ],
            stage1_recall_bias: 0.8,
        }
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
