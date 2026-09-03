//! Tests for the classifier stages (prompt building and parsing).

use jcode_classifier::stages::{build_stage1, build_stage2, parse_stage1_response, parse_stage2_response};
use jcode_classifier::{BlockCategory, ClassifierConfig, ClassifierInput, Decision, ToolCall};
use std::path::PathBuf;

fn create_test_input() -> ClassifierInput {
    ClassifierInput {
        user_message: "Write a function to add two numbers".to_string(),
        tool_call: ToolCall {
            name: "bash".to_string(),
            arguments: serde_json::json!({
                "command": "echo 'hello world'",
                "description": "Print hello world"
            }),
        },
        recent_history: Vec::new(),
        working_directory: PathBuf::from("/home/user/project"),
        config: ClassifierConfig::default(),
        turn_context: None,
    }
}

#[test]
fn test_build_stage1_prompt_format() {
    let input = create_test_input();
    let (system_prompt, user_message) = build_stage1(&input);
    
    // Check system prompt contains key instructions
    assert!(system_prompt.contains("safety classifier"));
    assert!(system_prompt.contains("REASON:"));
    assert!(system_prompt.contains("DECISION:"));
    assert!(system_prompt.contains("YES"));
    assert!(system_prompt.contains("NO"));
    
    // Check user message contains request and tool info
    assert!(user_message.contains("USER REQUEST:"));
    assert!(user_message.contains("Write a function"));
    assert!(user_message.contains("TOOL CALL:"));
    assert!(user_message.contains("bash"));
}

#[test]
fn test_build_stage1_with_trust_boundaries() {
    let mut input = create_test_input();
    input.config.trust_boundaries.git_remotes = vec![
        "github.com/myorg/*".to_string(),
        "gitlab.internal/*".to_string(),
    ];
    input.config.trust_boundaries.internal_services = vec![
        "api.internal.company.com".to_string(),
    ];
    
    let (_, user_message) = build_stage1(&input);
    
    assert!(user_message.contains("Trusted git remotes"));
    assert!(user_message.contains("github.com/myorg/*"));
    assert!(user_message.contains("gitlab.internal/*"));
    assert!(user_message.contains("Internal services"));
    assert!(user_message.contains("api.internal.company.com"));
}

#[test]
fn test_parse_stage1_response_yes() {
    let response = r#"
REASON: This is a safe, read-only operation that was requested by the user.
DECISION: YES
"#;
    
    let result = parse_stage1_response(response, 1).unwrap();
    
    match result.decision {
        Decision::Allow => {},
        Decision::Block { .. } => panic!("Expected Allow, got Block"),
    }
    assert_eq!(result.stage, 1);
    assert!(result.reason.contains("safe"));
}

#[test]
fn test_parse_stage1_response_no() {
    let response = r#"
REASON: This command would delete all files and cannot be undone.
DECISION: NO
"#;
    
    let result = parse_stage1_response(response, 1).unwrap();

    match result.decision {
        Decision::Allow => panic!("Expected Block, got Allow"),
        Decision::Block { detail, .. } => {
            // The block detail should carry the classifier's REASON so the
            // agent can see why the action was rejected.
            assert!(detail.contains("delete all files"));
        }
    }
}

#[test]
fn test_parse_stage1_response_block_with_detail() {
    let response = r#"
REASON: This would send user data to an external service.
DECISION: BLOCK: Exfiltrates data to untrusted destination
"#;
    
    let result = parse_stage1_response(response, 1).unwrap();
    
    match result.decision {
        Decision::Allow => panic!("Expected Block, got Allow"),
        Decision::Block { category, detail } => {
            assert_eq!(category, BlockCategory::DestroyExfiltrate);
            assert!(detail.contains("Exfiltrates data"));
        }
    }
}

#[test]
fn test_parse_stage1_invalid_response() {
    let response = "This is not a valid response format";
    let result = parse_stage1_response(response, 1);
    
    assert!(result.is_err());
}

#[test]
fn test_parse_stage2_response_allow() {
    let response = r#"
REASON: After careful analysis, this action is authorized and safe.
DECISION: ALLOW
"#;
    
    let result = parse_stage2_response(response, 2).unwrap();
    
    match result.decision {
        Decision::Allow => {},
        Decision::Block { .. } => panic!("Expected Allow, got Block"),
    }
    assert_eq!(result.stage, 2);
}

#[test]
fn test_parse_stage2_response_block() {
    let response = r#"
REASON: This would force push to main and cannot be recovered.
DECISION: BLOCK: Force push would destroy history
"#;
    
    let result = parse_stage2_response(response, 2).unwrap();
    
    match result.decision {
        Decision::Allow => panic!("Expected Block, got Allow"),
        Decision::Block { category, detail } => {
            assert!(detail.contains("Force push"));
        }
    }
}

#[test]
fn test_parse_stage1_case_insensitive() {
    let response = r#"
REASON: Test reason
decision: YES
"#;
    
    let result = parse_stage1_response(response, 1).unwrap();
    match result.decision {
        Decision::Allow => {},
        Decision::Block { .. } => panic!("Expected Allow"),
    }
}

#[test]
fn test_parse_stage1_multiline_reason() {
    let response = r#"
REASON: This is a multi-line reason that explains
why the decision was made in detail.
DECISION: YES
"#;
    
    let result = parse_stage1_response(response, 1).unwrap();
    assert!(result.reason.contains("multi-line"));
}

#[test]
fn test_build_stage2_prompt_format() {
    let input = create_test_input();
    let (system_prompt, user_message) = build_stage2(&input);
    
    // Check Stage 2 has more detailed evaluation steps
    assert!(system_prompt.contains("careful judgment"));
    assert!(system_prompt.contains("ANALYZE"));
    assert!(system_prompt.contains("AUTHORIZATION"));
    assert!(system_prompt.contains("BLAST_RADIUS"));
    assert!(system_prompt.contains("ALLOW"));
    assert!(system_prompt.contains("BLOCK"));
}
