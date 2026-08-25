//! Configuration for the AI classifier.

use serde::{Deserialize, Serialize};

/// Configuration for the classifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifierConfig {
    /// Model route to use for classification.
    /// 
    /// - None: Use session's current model (default)
    /// - Some("claude"): Use default Claude model
    /// - Some("openai"): Use default OpenAI model
    /// - Some("claude:claude-sonnet-4-20250514"): Use specific model
    #[serde(default)]
    pub model_route: Option<String>,

    /// Trust boundaries.
    #[serde(default)]
    pub trust_boundaries: TrustBoundaries,

    /// Which block rules are active.
    #[serde(default)]
    pub block_rules: BlockRules,

    /// Exceptions to block rules.
    #[serde(default)]
    pub allow_exceptions: Vec<String>,

    /// Stage 1 recall bias (0.0 = balanced, 1.0 = catch everything).
    /// 
    /// Higher values mean fewer false negatives (catch more dangerous actions)
    /// at the cost of more false positives (blocking legitimate actions).
    #[serde(default = "default_recall_bias")]
    pub stage1_recall_bias: f32,
}

fn default_recall_bias() -> f32 {
    0.8
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            model_route: None,
            trust_boundaries: TrustBoundaries::default(),
            block_rules: BlockRules::default(),
            allow_exceptions: vec![
                "package_install_from_lockfile".to_string(),
                "standard_credential_flows".to_string(),
                "push_to_working_branch".to_string(),
            ],
            stage1_recall_bias: 0.8,
        }
    }
}

/// Trust boundaries configuration.
///
/// Defines what the classifier considers "trusted" in the user's environment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustBoundaries {
    /// The git repository you're working in.
    #[serde(default = "default_true")]
    pub working_directory: bool,

    /// Trusted git remote patterns (glob patterns).
    #[serde(default)]
    pub git_remotes: Vec<String>,

    /// Internal services/domains.
    #[serde(default)]
    pub internal_services: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// Block rules configuration.
///
/// Categories of actions that the classifier will block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlockRules {
    /// Permanent data destruction or exfiltration.
    #[serde(default = "default_true")]
    pub destroy_exfiltrate: bool,

    /// Weakening system security.
    #[serde(default = "default_true")]
    pub degrade_security: bool,

    /// Accessing untrusted infrastructure.
    #[serde(default = "default_true")]
    pub cross_boundaries: bool,

    /// Skipping review safeguards.
    #[serde(default = "default_true")]
    pub bypass_review: bool,
}

/// Permission modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Always ask for confirmation (existing behavior).
    #[default]
    Manual,
    /// Use AI classifier for decisions.
    Auto,
}

/// Top-level permissions configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionsConfig {
    /// Default permission mode for new sessions.
    #[serde(default)]
    pub default_mode: PermissionMode,

    /// Auto mode configuration.
    #[serde(default)]
    pub auto_mode: AutoModeConfig,
}

/// Auto mode configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AutoModeConfig {
    /// Model to use for classification.
    #[serde(default)]
    pub classifier_model: Option<String>,

    /// Stage 1 fast filter settings.
    #[serde(default)]
    pub stage1: Stage1Config,

    /// Trust boundaries.
    #[serde(default)]
    pub trust_boundaries: TrustBoundaries,

    /// Block rules.
    #[serde(default)]
    pub block_rules: BlockRules,

    /// Allow exceptions.
    #[serde(default)]
    pub allow_exceptions: Vec<String>,
}

/// Stage 1 configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage1Config {
    /// Recall bias for Stage 1.
    #[serde(default = "default_recall_bias")]
    pub recall_bias: f32,
}

impl Default for Stage1Config {
    fn default() -> Self {
        Self {
            recall_bias: 0.8,
        }
    }
}

impl From<&PermissionsConfig> for ClassifierConfig {
    fn from(permissions: &PermissionsConfig) -> Self {
        Self {
            model_route: permissions.auto_mode.classifier_model.clone(),
            trust_boundaries: permissions.auto_mode.trust_boundaries.clone(),
            block_rules: permissions.auto_mode.block_rules.clone(),
            allow_exceptions: permissions.auto_mode.allow_exceptions.clone(),
            stage1_recall_bias: permissions.auto_mode.stage1.recall_bias,
        }
    }
}
