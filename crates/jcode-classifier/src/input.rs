//! Input building for the classifier.

use crate::config::ClassifierConfig;
use crate::{ClassifierInput, ToolCall};
use jcode_message_types::Message;
use std::path::PathBuf;

/// The user's request text for the current turn.
///
/// This is the literal text the user sent that the agent is acting on. It is the
/// primary signal the classifier uses to decide whether a tool call is
/// authorized: an action aligned with this request is far more likely to be
/// allowed than one the user never asked for.
#[derive(Debug, Clone, Default)]
pub struct TurnContext {
    /// The user's request text for the current turn (what they asked for).
    pub user_message: Option<String>,
    /// Recent conversation history (truncated to what fits the prompt).
    pub recent_history: Vec<Message>,
}

impl TurnContext {
    /// Empty turn context (no user message / no history available).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Construct from a user message and history.
    pub fn new(user_message: Option<String>, recent_history: Vec<Message>) -> Self {
        Self {
            user_message,
            recent_history,
        }
    }

    /// Build a honorific-free single-string summary of the turn context for
    /// prompts that expect a flat "USER REQUEST:" line. Falls back to an empty
    /// string when no user message is available.
    pub fn user_request_line(&self) -> String {
        self.user_message.clone().unwrap_or_default()
    }
}

/// Builder for ClassifierInput.
#[derive(Debug)]
pub struct ClassifierInputBuilder {
    user_message: Option<String>,
    tool_call: Option<ToolCall>,
    recent_history: Vec<Message>,
    working_directory: Option<PathBuf>,
    config: ClassifierConfig,
    turn_context: Option<TurnContext>,
}

impl ClassifierInputBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            user_message: None,
            tool_call: None,
            recent_history: Vec::new(),
            working_directory: None,
            config: ClassifierConfig::default(),
            turn_context: None,
        }
    }

    /// Set the user message.
    pub fn user_message(mut self, msg: impl Into<String>) -> Self {
        self.user_message = Some(msg.into());
        self
    }

    /// Set the tool call.
    pub fn tool_call(mut self, name: impl Into<String>, arguments: serde_json::Value) -> Self {
        self.tool_call = Some(ToolCall {
            name: name.into(),
            arguments,
        });
        self
    }

    /// Add a message to history.
    pub fn add_history(mut self, msg: Message) -> Self {
        self.recent_history.push(msg);
        self
    }

    /// Set the working directory.
    pub fn working_directory(mut self, dir: PathBuf) -> Self {
        self.working_directory = Some(dir);
        self
    }

    /// Set the configuration.
    pub fn config(mut self, config: ClassifierConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the full turn context (user request + history).
    pub fn turn_context(mut self, turn_context: TurnContext) -> Self {
        self.turn_context = Some(turn_context);
        self
    }

    /// Build the ClassifierInput.
    pub fn build(self) -> Result<ClassifierInput, ClassifierInputError> {
        Ok(ClassifierInput {
            user_message: self.user_message.ok_or(ClassifierInputError::MissingUserMessage)?,
            tool_call: self.tool_call.ok_or(ClassifierInputError::MissingToolCall)?,
            recent_history: self.recent_history,
            working_directory: self.working_directory
                .unwrap_or_else(|| PathBuf::from(".")),
            config: self.config,
            turn_context: self.turn_context,
        })
    }
}

impl Default for ClassifierInputBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors when building ClassifierInput.
#[derive(Debug)]
pub enum ClassifierInputError {
    MissingUserMessage,
    MissingToolCall,
}

impl std::fmt::Display for ClassifierInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClassifierInputError::MissingUserMessage => {
                write!(f, "user_message is required")
            }
            ClassifierInputError::MissingToolCall => {
                write!(f, "tool_call is required")
            }
        }
    }
}

impl std::error::Error for ClassifierInputError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_success() {
        let input = ClassifierInputBuilder::new()
            .user_message("Delete the old files")
            .tool_call("bash", serde_json::json!({"command": "rm -rf old/"}))
            .working_directory(PathBuf::from("/project"))
            .build()
            .unwrap();

        assert_eq!(input.user_message, "Delete the old files");
        assert_eq!(input.tool_call.name, "bash");
    }

    #[test]
    fn test_build_missing_user_message() {
        let result = ClassifierInputBuilder::new()
            .tool_call("bash", serde_json::json!({"command": "ls"}))
            .build();

        assert!(result.is_err());
    }
}
