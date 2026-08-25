//! Decision types for the classifier.

use serde::{Deserialize, Serialize};

/// A decision from the classifier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Decision {
    /// The action is allowed.
    Allow,
    /// The action is blocked.
    Block {
        /// Category of the block.
        category: BlockCategory,
        /// Detailed reason for the block.
        detail: String,
    },
}

impl std::fmt::Display for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Decision::Allow => write!(f, "ALLOW"),
            Decision::Block { category, detail } => {
                write!(f, "BLOCK: {} - {}", category, detail)
            }
        }
    }
}

/// Categories of actions that can be blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockCategory {
    /// Permanent data destruction or exfiltration.
    DestroyExfiltrate,
    /// Weakening system security.
    DegradeSecurity,
    /// Accessing untrusted infrastructure.
    CrossBoundaries,
    /// Skipping review safeguards.
    BypassReview,
}

impl std::fmt::Display for BlockCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockCategory::DestroyExfiltrate => write!(f, "destroy/exfiltrate"),
            BlockCategory::DegradeSecurity => write!(f, "degrade security"),
            BlockCategory::CrossBoundaries => write!(f, "cross boundaries"),
            BlockCategory::BypassReview => write!(f, "bypass review"),
        }
    }
}

impl BlockCategory {
    /// Parse a block category from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "destroy_exfiltrate" | "destroy" | "exfiltrate" | "destroy/exfiltrate" => {
                Some(BlockCategory::DestroyExfiltrate)
            }
            "degrade_security" | "degrade" | "security" | "degrade security" => {
                Some(BlockCategory::DegradeSecurity)
            }
            "cross_boundaries" | "cross" | "boundaries" | "cross boundaries" => {
                Some(BlockCategory::CrossBoundaries)
            }
            "bypass_review" | "bypass" | "review" | "bypass review" => {
                Some(BlockCategory::BypassReview)
            }
            _ => None,
        }
    }
}
