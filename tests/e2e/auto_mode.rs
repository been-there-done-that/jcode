//! Auto-mode permission demos.
//!
//! One deterministic scenario walking `request_permission` through:
//!   1. Manual mode -> queues for human review -> human approves
//!   2. Auto mode   -> AI classifier approves a safe action
//!   3. Auto mode   -> AI classifier rejects a destructive action
//!   4. Auto mode   -> broken classifier falls back to human review
//!
//! Run with:
//!   cargo test --test e2e -- --nocapture --test-threads=1 auto_mode_demo

use crate::mock_provider::MockProvider;
use crate::test_support::*;
use jcode::message::{Message, StreamEvent};
use jcode::safety::{PermissionMode, PermissionRequest, SafetySystem, Urgency};
use jcode::tool::ambient::RequestPermissionTool;
use jcode::tool::{Tool, ToolContext, ToolExecutionMode};
use std::sync::Arc;

fn ctx(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: format!("msg_{session}"),
        tool_call_id: format!("call_{session}"),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn stage1_response(reason: &str, decision: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(format!("REASON: {reason}\nDECISION: {decision}")),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        },
    ]
}

/// Call the real request_permission tool and return its rendered output text.
async fn request(
    tool: &RequestPermissionTool,
    session: &str,
    action: &str,
    description: &str,
    rationale: &str,
) -> String {
    let input = serde_json::json!({
        "action": action,
        "description": description,
        "rationale": rationale,
        "urgency": "normal",
        "wait": false,
    });
    let output = tool
        .execute(input, ctx(session))
        .await
        .expect("request_permission tool should succeed");
    output.output
}

#[tokio::test]
async fn auto_mode_demo() -> Result<()> {
    let _env = setup_test_env()?;

    // One shared classifier provider whose scripted responses are consumed in
    // order: allow, block, garbage. Mode switches go through the live
    // SafetySystem so every scenario below observes them.
    let classifier_provider = MockProvider::new();
    classifier_provider.queue_response(stage1_response(
        "Running the project's own test suite is reversible and aligned with the user request",
        "YES",
    ));
    classifier_provider.queue_response(stage1_response(
        "Dropping a production table permanently destroys data the user never asked to delete",
        "NO",
    ));
    // Third call: empty model response -> parser error -> fallback to queue.
    classifier_provider.queue_response(vec![StreamEvent::MessageEnd {
        stop_reason: Some("end_turn".to_string()),
    }]);

    let safety = Arc::new(SafetySystem::new());
    // Grab the prompt-capture handle before the provider is moved into the
    // classifier, so scenario 2 can assert the classifier was consulted.
    let classifier_prompts = classifier_provider.captured_system_prompts.clone();
    safety.init_classifier(Arc::new(classifier_provider) as Arc<dyn Provider>, None);
    jcode::tool::ambient::init_safety_system(safety.clone());

    let tool = RequestPermissionTool::new();
    let session = "demo_session";

    jcode::tool::ambient::register_ambient_session(session);
    let baseline_pending = safety.pending_requests().len();

    // -----------------------------------------------------------------
    println!("\n========== SCENARIO 1: Manual mode ==========");
    safety.set_mode(PermissionMode::Manual);
    let out = request(
        &tool,
        session,
        "bash",
        "Run cargo test for the auth module",
        "Verify the fix compiles and passes",
    )
    .await;
    println!("tool result -> {out}");
    assert!(out.contains("queued"), "manual mode must queue: {out}");
    assert_eq!(
        safety.pending_requests().len(),
        baseline_pending + 1,
        "exactly one request waiting for a human"
    );

    // Human reviews and approves (same path the permissions TUI uses).
    let pending_id = safety.pending_requests()[baseline_pending].id.clone();
    safety
        .record_decision(&pending_id, true, "permissions_tui", Some("ship it".into()))
        .unwrap();
    println!("human decision -> approved request {pending_id}");
    assert_eq!(safety.pending_requests().len(), baseline_pending);

    // -----------------------------------------------------------------
    println!("\n========== SCENARIO 2: Auto mode, safe action ==========");
    safety.set_mode(PermissionMode::Auto);
    let out = request(
        &tool,
        session,
        "bash",
        "Run cargo test -p jcode-base",
        "Confirm refactor did not break behavior",
    )
    .await;
    println!("tool result -> {out}");
    assert!(out.contains("auto-approved"), "expected auto-approval: {out}");
    assert_eq!(
        safety.pending_requests().len(),
        baseline_pending,
        "approval must not leave anything queued"
    );
    // Prove the classifier (not a human) made this decision.
    {
        let prompts = classifier_prompts.lock().unwrap();
        assert!(!prompts.is_empty(), "classifier must have been consulted");
        let stage1 = &prompts[0];
        assert!(
            stage1.contains("safety classifier") && stage1.contains("DECISION: YES or NO"),
            "stage-1 prompt should be the classifier evaluation prompt"
        );
    }

    // -----------------------------------------------------------------
    println!("\n========== SCENARIO 3: Auto mode, dangerous action ==========");
    let out = request(
        &tool,
        session,
        "bash",
        "Drop the production users table to reset auth state",
        "Auth state seems stuck",
    )
    .await;
    println!("tool result -> {out}");
    assert!(out.contains("auto-denied"), "expected auto-denial: {out}");
    assert_eq!(
        safety.pending_requests().len(),
        baseline_pending,
        "deny-and-continue keeps the queue clean"
    );

    // -----------------------------------------------------------------
    println!("\n========== SCENARIO 4: Auto mode, classifier unavailable ==========");
    let out = request(
        &tool,
        session,
        "edit",
        "Rewrite token validation in src/auth.rs",
        "Token expiry check is wrong",
    )
    .await;
    println!("tool result -> {out}");
    assert!(
        out.contains("queued"),
        "classifier failure must fall back to human review: {out}"
    );

    // Clean up the queued request so the persisted queue stays tidy.
    if let Some(req) = safety.pending_requests().last() {
        let id = req.id.clone();
        safety.record_decision(&id, false, "demo_cleanup", None).unwrap();
        println!("cleanup -> removed stale demo request {id}");
    }

    // -----------------------------------------------------------------
    println!("\n========== SCENARIO 5: End-to-end through the agent loop ==========");
    // Full agent turn in Manual mode: model emits request_permission, tool
    // queues it, model sees the queue confirmation as its tool result.
    let provider = MockProvider::new();
    let tool_input = serde_json::json!({
        "action": "create_pull_request",
        "description": "Create PR for auth fixes",
        "rationale": "3 failing auth tests fixed",
        "urgency": "high",
        "wait": false,
    })
    .to_string();
    provider.queue_response(vec![
        StreamEvent::ToolUseStart {
            id: "tool_perm_e2e".to_string(),
            name: "request_permission".to_string(),
        },
        StreamEvent::ToolInputDelta(tool_input),
        StreamEvent::ToolUseEnd,
        StreamEvent::MessageEnd {
            stop_reason: Some("tool_use".to_string()),
        },
    ]);
    provider.queue_response(vec![
        StreamEvent::TextDelta("Queued for your review.".to_string()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        },
    ]);

    safety.set_mode(PermissionMode::Manual);
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let registry = Registry::new(provider.clone()).await;
    registry.register_ambient_tools().await;
    let mut agent = Agent::new(provider, registry);
    let agent_session = agent.session_id().to_string();
    jcode::tool::ambient::register_ambient_session(agent_session.clone());
    let _response = agent.run_once_capture("Open a PR for the auth fixes").await?;
    jcode::tool::ambient::unregister_ambient_session(&agent_session);

    let pending_after_agent = safety.pending_requests();
    let agent_request = pending_after_agent
        .iter()
        .find(|r| r.action == "create_pull_request")
        .expect("agent-emitted request should be pending");
    println!(
        "queued from agent loop -> id={} action={} urgency={:?}",
        agent_request.id, agent_request.action, agent_request.urgency
    );
    assert_eq!(agent_request.urgency, Urgency::High);

    // Approve it the way `jcode permissions` / debug socket would.
    safety
        .record_decision(&agent_request.id, true, "debug_socket", None)
        .unwrap();

    // Sanity: decisions landed in history with the right `via` labels.
    // (history.json lives under the temp JCODE_HOME)
    let history_path = jcode::storage::jcode_dir()
        .unwrap()
        .join("safety")
        .join("history.json");
    let raw = std::fs::read_to_string(&history_path).unwrap_or_default();
    println!("decision log excerpt -> {}", raw.chars().take(160).collect::<String>());
    assert!(raw.contains("debug_socket") && raw.contains("permissions_tui"));

    let _: Option<PermissionRequest> = None;
    let _: Vec<Message> = Vec::new(); // keep imports honest if code shifts
    jcode::tool::ambient::unregister_ambient_session(session);

    // -----------------------------------------------------------------
    println!("\n========== SCENARIO 6: Central gate blocks effectful tools ==========");
    // The Auto Mode classifier must gate *real* tool execution (bash, write,
    // ...) through Registry::execute, not just the ambient request_permission
    // tool. Here the user says "talk to the DB with python, but don't run
    // delete queries" -> a delete command arriving via bash must be blocked.
    safety.set_mode(PermissionMode::Auto);

    // Fresh classifier provider: 1) allow a select query, 2) block the delete.
    let gate_provider = MockProvider::new();
    gate_provider.queue_response(stage1_response(
        "Read-only SELECT against the analytics table matches the user's request",
        "YES",
    ));
    gate_provider.queue_response(stage1_response(
        "DROP TABLE destroys data; the user explicitly said not to run delete queries",
        "NO",
    ));
    safety.init_classifier(
        Arc::new(gate_provider) as Arc<dyn Provider>,
        None,
    );
    jcode::tool::ambient::init_safety_system(safety.clone());

    // Register effectful test tools named exactly like the real ones so the
    // AUTO_MODE_EFFECTFUL_TOOLS gate triggers.
    let gate_provider_for_registry = MockProvider::new();
    let gate_registry = Registry::new(Arc::new(gate_provider_for_registry) as Arc<dyn Provider>).await;
    gate_registry
        .register(
            "bash".to_string(),
            Arc::new(GateTool {
                name: "bash",

            }),
        )
        .await;
    gate_registry
        .register(
            "write".to_string(),
            Arc::new(GateTool {
                name: "write",

            }),
        )
        .await;

    let turn_context = jcode_classifier::TurnContext::new(
        Some("use python to talk to the database, but do not run any delete queries".to_string()),
        Vec::new(),
    );
    gate_registry.set_turn_context(Some(turn_context)).await;

    // 1) A benign SELECT command is allowed and the tool actually runs.
    let select_out = gate_registry
        .execute(
            "bash",
            serde_json::json!({ "command": "python -c \"SELECT * FROM users\"" }),
            ctx("gate_session"),
        )
        .await;
    println!("select command -> {select_out:?}");
    assert!(select_out.is_ok(), "read-only command must be allowed");
    assert_eq!(select_out.unwrap().output, "ran:bash");

    // 2) A DROP/DELETE command is blocked by the classifier and never reaches
    //    the underlying tool (fail-closed).
    let delete_out = gate_registry
        .execute(
            "bash",
            serde_json::json!({ "command": "python -c \"DROP TABLE users\"" }),
            ctx("gate_session"),
        )
        .await;
    println!("delete command -> {delete_out:?}");
    assert!(delete_out.is_err(), "destructive command must be blocked");
    let err = delete_out.unwrap_err().to_string();
    assert!(
        err.contains("Auto Mode blocked"),
        "gate must report an Auto Mode block: {err}"
    );

    println!("\n========== SCENARIO 7: Constraint in history (not last user msg) ==========");
    // The key correctness requirement: a user constraint stated *earlier* in the
    // turn (so it is NOT the latest user message) must still gate later tool
    // calls. Stage 1 (the fast filter) must see the history, otherwise it would
    // confidently ALLOW a destructive command and stop, never reaching Stage 2.
    let history_provider = MockProvider::new();
    // The classifier's Stage 1 must now consult history and BLOCK, since the
    // history contains "do not run delete queries".
    history_provider.queue_response(stage1_response(
        "History shows the user said not to run delete queries; DROP TABLE violates that",
        "NO",
    ));
    safety.init_classifier(
        Arc::new(history_provider) as Arc<dyn Provider>,
        None,
    );
    jcode::tool::ambient::init_safety_system(safety.clone());

    let history_registry = Registry::new(Arc::new(MockProvider::new()) as Arc<dyn Provider>).await;
    history_registry
        .register("bash".to_string(), Arc::new(GateTool { name: "bash" }))
        .await;

    // Constraint is in the *history*, while the current turn's user message is a
    // benign follow-up. This is the exact shape that previously slipped through.
    let history_turn_context = jcode_classifier::TurnContext::new(
        Some("ok now generate the weekly report from the data".to_string()),
        vec![
            jcode::message::Message::user("use python to query the database"),
            jcode::message::Message::assistant_text("Sure, which queries do you need?"),
            jcode::message::Message::user("query away, but do not run any delete queries"),
        ],
    );
    history_registry
        .set_turn_context(Some(history_turn_context))
        .await;

    let hidden_delete = history_registry
        .execute(
            "bash",
            serde_json::json!({ "command": "python -c \"DROP TABLE users\"" }),
            ctx("gate_session"),
        )
        .await;
    println!("history-constrained delete -> {hidden_delete:?}");
    assert!(
        hidden_delete.is_err(),
        "a constraint in history must still block destructive commands"
    );
    let hist_err = hidden_delete.unwrap_err().to_string();
    assert!(
        hist_err.contains("Auto Mode blocked"),
        "gate must report an Auto Mode block from history: {hist_err}"
    );

    println!("\nAll scenarios behaved as expected.");
    Ok(())
}

/// Effectful test tool used in SCENARIO 6 to prove the central classifier gate
/// prevents execution of blocked commands. The tool itself is a no-op that
/// records nothing; what matters is whether `execute` is reached at all.
struct GateTool {
    name: &'static str,
}

#[async_trait::async_trait]
impl jcode::tool::Tool for GateTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "Test effectful tool for the central Auto Mode gate."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["command"],
            "properties": { "command": { "type": "string" } }
        })
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: jcode::tool::ToolContext,
    ) -> anyhow::Result<jcode::tool::ToolOutput> {
        // If we reach here, the gate let the command through.
        Ok(jcode::tool::ToolOutput::new(format!("ran:{}", self.name)))
    }
}
