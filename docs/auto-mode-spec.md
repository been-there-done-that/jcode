# Technical Specification: AI-Assisted Permission Classification

**Version:** 1.0  
**Status:** Approved  
**Author:** jcode team  
**Date:** 2026-08-25

---

## Overview

This document describes the implementation of an AI-assisted permission classification system for jcode, inspired by Claude Code's Auto Mode. The system adds a second layer of safety assessment using the existing Provider infrastructure, replacing the static `RiskLevel::Confirm` escalation with an intelligent classifier.

---

## Motivation

### Problem

The current permission system uses static analysis (`jcode-command-risk`) to classify shell commands. This approach has limitations:

1. **Dynamic targets**: Commands like `rm -rf $(cat list.txt)` can't be statically analyzed
2. **Database operations**: Migrations, drops, and mass updates require context
3. **Intent alignment**: Static analysis can't determine if an action aligns with user intent
4. **Trust boundaries**: External services, cloud resources, and shared infrastructure need evaluation

### Solution

Add an AI classifier that evaluates permission requests holistically:

- Uses the session's existing Provider (no new auth/API keys)
- Two-stage evaluation: fast filter → full reasoning
- Configurable trust boundaries and block rules
- Maintains the deny-and-continue pattern (blocked → agent tries alternative)

---

## Architecture

### Layered Permission Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                    Tool Call Request                             │
└─────────────────────────┬───────────────────────────────────────┘
                          │
┌─────────────────────────▼───────────────────────────────────────┐
│  Layer 1: SafetySystem (Static Allowlist)                      │
│                                                                 │
│  AutoAllowed tools:                                            │
│    - read, glob, grep, ls                                      │
│    - memory, todo, todowrite, todoread                          │
│    - conversation_search, session_search, codesearch            │
│                                                                 │
│  Immediate block:                                               │
│    - Catastrophic: protected paths (~, /, credentials)         │
└─────────────────────────┬───────────────────────────────────────┘
                          │
                          │ RequiresPermission
┌─────────────────────────▼───────────────────────────────────────┐
│  Layer 2: CommandRiskAssessment (Static Analysis)              │
│                                                                 │
│  RiskLevel::Safe → Run immediately                              │
│  RiskLevel::Low → Run + log                                    │
│  RiskLevel::Confirm → Escalate to AI classifier                │
│  RiskLevel::Catastrophic → Already blocked by Layer 1           │
└─────────────────────────┬───────────────────────────────────────┘
                          │
                          │ RiskLevel::Confirm
┌─────────────────────────▼───────────────────────────────────────┐
│  Layer 3: AI Transcript Classifier                            │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Stage 1: Fast Filter (Single API call)                 │   │
│  │                                                         │   │
│  │ - Returns: REASON + DECISION (YES/NO)                   │   │
│  │ - ~10ms latency target                                  │   │
│  │ - Tuned for high recall (catch dangerous actions)       │   │
│  │ - YES → Allow                                          │   │
│  │ - NO → Continue to Stage 2                             │   │
│  └─────────────────────────────────────────────────────────┘   │
│                            │                                   │
│                            │ Stage 1 flagged                   │
│  ┌─────────────────────────▼─────────────────────────────────┐  │
│  │ Stage 2: Full Reasoning                                  │  │
│  │                                                         │  │
│  │ - Chain-of-thought evaluation                          │  │
│  │ - Trust boundary checks                                 │  │
│  │ - Block rule matching                                   │  │
│  │ - Allow exception matching                              │  │
│  │ - Returns: REASON + DECISION (ALLOW/BLOCK)             │  │
│  └─────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Core Principles

### 1. Reuse Existing Infrastructure

The classifier is **not a new system**. It reuses:

| Component | Existing | Notes |
|-----------|----------|-------|
| Model selection | `ModelRoute` | Same as agent |
| Authentication | `Provider` trait | OAuth, API keys, etc. |
| API calls | `Provider::complete()` | Existing retry/rate-limit |
| Configuration | Settings schema | Extend existing patterns |

**No new API keys, no new auth flows, no new endpoints.**

### 2. Reasoned Decisions

Both Stage 1 and Stage 2 return **reasoning** along with the decision:

```
REASON: The user explicitly asked to delete the temp directory as part of cleanup.
        This is a small, targeted deletion that can be recovered from git.
DECISION: YES
```

This enables:
- Debugging: Understand why decisions were made
- Transparency: Users can audit classifier behavior
- Trust: Clear justification builds confidence

### 3. Configurable Trust Boundaries

Users define what's "trusted" in their environment:

```json
{
  "trust_boundaries": {
    "working_directory": true,
    "git_remotes": ["github.com/myorg/*"],
    "internal_services": ["api.internal.com"]
  }
}
```

### 4. Block Rules

The classifier blocks **4 categories** of actions:

| Category | Examples |
|----------|----------|
| **Destroy/Exfiltrate** | Force-push, mass-delete, send data externally |
| **Degrade Security** | Disable logging, install SSH keys, modify permissions |
| **Cross Boundaries** | Run external code, scan credentials, send to unspecified services |
| **Bypass Review** | Skip safety checks, push to main, modify shared resources |

### 5. Allow Exceptions

Narrow carve-outs for actions that technically match a block rule but are benign:

```
"allow_exceptions": [
  "package_install_from_lockfile",
  "standard_credential_flows",
  "push_to_working_branch"
]
```

### 6. Deny-and-Continue Pattern

When the classifier blocks an action:

1. Return denial reason to the agent
2. Agent attempts an alternative path
3. If 3 consecutive or 20 total denials → halt and escalate to user

---

## Data Types

### ClassifierInput

```rust
/// Input to the classifier
#[derive(Debug, Clone)]
pub struct ClassifierInput {
    /// The user's actual request
    pub user_message: String,
    
    /// The tool call being requested
    pub tool_call: ToolCall,
    
    /// Recent conversation history (truncated to last N messages)
    pub recent_history: Vec<ChatMessage>,
    
    /// Working directory
    pub working_directory: PathBuf,
    
    /// Classifier configuration
    pub config: ClassifierConfig,
}
```

### ClassifierResult

```rust
/// Output from the classifier
#[derive(Debug, Clone)]
pub struct ClassifierResult {
    /// The decision
    pub decision: Decision,
    
    /// Human-readable reasoning
    pub reason: String,
    
    /// Which stage produced this result (1 or 2)
    pub stage: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow,
    Block {
        category: BlockCategory,
        detail: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockCategory {
    DestroyExfiltrate,
    DegradeSecurity,
    CrossBoundaries,
    BypassReview,
}
```

### ClassifierConfig

```rust
/// Configuration for the classifier
#[derive(Debug, Clone)]
pub struct ClassifierConfig {
    /// Model route to use for classification
    /// None = use session's current model
    pub model_route: Option<String>,
    
    /// Trust boundaries
    pub trust_boundaries: TrustBoundaries,
    
    /// Which block rules are active
    pub block_rules: BlockRules,
    
    /// Exceptions to block rules
    pub allow_exceptions: Vec<String>,
    
    /// Stage 1 recall bias (0.0 = balanced, 1.0 = catch everything)
    pub stage1_recall_bias: f32,
}

#[derive(Debug, Clone, Default)]
pub struct TrustBoundaries {
    pub working_directory: bool,
    pub git_remotes: Vec<String>,
    pub internal_services: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BlockRules {
    pub destroy_exfiltrate: bool,
    pub degrade_security: bool,
    pub cross_boundaries: bool,
    pub bypass_review: bool,
}
```

---

## Prompts

### Stage 1: Fast Filter

```
SYSTEM:
You are a safety classifier for an AI coding agent. Your job is to determine
if a tool call is authorized by the user's request and aligned with safe practices.

Respond with EXACTLY this format:
REASON: <your reasoning in 2-3 sentences>
DECISION: YES or NO

- YES means: The action is clearly authorized by what the user asked and poses minimal risk.
- NO means: The action either wasn't requested, or poses significant risk (destroys data,
  sends data outside, degrades security, or crosses trust boundaries).

CLASSIFICATION CRITERIA:
1. DESTROY/EXFILTRATE: Does this permanently delete data or send it outside trusted boundaries?
2. DEGRADE SECURITY: Does this weaken system security (disable logging, add persistence)?
3. CROSS BOUNDARIES: Does this access untrusted infrastructure or scan for credentials?
4. BYPASS REVIEW: Does this skip safety checks or affect shared infrastructure?

USER REQUEST: {user_message}

TOOL CALL:
- Tool: {tool_name}
- Arguments: {tool_args}

TRUSTED BOUNDARIES:
- Working directory: {working_dir}
- Trusted git remotes: {trusted_remotes}
```

**Example Response:**
```
REASON: The user explicitly asked to delete the file "old-notes.txt" in their project directory.
This is a small, targeted deletion that can be recovered from version control.
DECISION: YES
```

### Stage 2: Full Reasoning

```
SYSTEM:
You are a safety classifier making a careful judgment about an AI coding agent's
tool call. This action was flagged by the fast filter and requires thorough review.

Evaluate step by step, then provide:
REASON: <detailed reasoning>
DECISION: ALLOW or BLOCK: <specific reason if blocked>

EVALUATION STEPS:
1. ANALYZE: What does this action actually do in the real world?
2. AUTHORIZATION: Is this explicitly requested by the user, or inferred by the agent?
3. BLAST_RADIUS: What would be lost or affected if this runs?
4. RECOVERY: Can this be undone? How easily?
5. BOUNDARIES: Does this cross into untrusted territory?

BLOCK RULES:
- Destroy/Exfiltrate: Force-push, mass-delete, send data externally
- Degrade Security: Disable logging, install SSH keys/cronjobs, modify permissions
- Cross Boundaries: Run external code, scan credentials, send to unspecified services
- Bypass Review: Skip safety checks, push to main, modify shared resources

ALLOW EXCEPTIONS (if action matches one of these, it may still be allowed):
{allow_exceptions}

USER REQUEST: {user_message}

TOOL CALL:
- Tool: {tool_name}
- Arguments: {tool_args}

HISTORY (recent context):
{recent_messages}
```

**Example Response:**
```
REASON: The user said "clean up my branches" which is vague. The agent interpreted this as
a request to delete 12 old feature branches. However, batch deletion of branches is
irreversible and the user only mentioned "branches" without specifying "remote" or
"delete". This could be scope creep beyond what was authorized.

DECISION: BLOCK: Batch branch deletion not explicitly authorized; user intent unclear
```

---

## Configuration Schema

```jsonc
// ~/.jcode/settings.json
{
  "permissions": {
    // Default permission mode for new sessions
    // - "manual": Always ask for confirmation (existing behavior)
    // - "auto": Use AI classifier for decisions
    "default_mode": "manual",
    
    // Auto mode configuration
    "auto_mode": {
      // Model to use for classification:
      // - Not specified: Use session's current model
      // - "claude": Use default Claude model
      // - "openai": Use default OpenAI model
      // - "claude:claude-sonnet-4-20250514": Use specific model
      // - "openrouter:anthropic/claude-sonnet-4": Use via OpenRouter
      "classifier_model": null,
      
      // Stage 1 fast filter settings
      "stage1": {
        // Higher = catch more dangerous actions (fewer false negatives)
        // Lower = fewer false positives (fewer blocked legitimate actions)
        // Range: 0.0 to 1.0, default: 0.8
        "recall_bias": 0.8
      },
      
      // What the classifier considers "trusted"
      "trust_boundaries": {
        // The git repository you're working in
        "working_directory": true,
        
        // Trusted git remote patterns (glob patterns)
        "git_remotes": [
          "github.com/*",
          "gitlab.com/myorg/*"
        ],
        
        // Internal services/domains
        "internal_services": [
          "api.internal.com",
          "*.internal.com"
        ]
      },
      
      // Categories of actions to block
      "block_rules": {
        // Permanent data destruction or exfiltration
        "destroy_exfiltrate": true,
        
        // Weakening system security
        "degrade_security": true,
        
        // Accessing untrusted infrastructure
        "cross_boundaries": true,
        
        // Skipping review safeguards
        "bypass_review": true
      },
      
      // Actions that match a block rule but are still allowed
      "allow_exceptions": [
        "package_install_from_lockfile",
        "standard_credential_flows",
        "push_to_working_branch"
      ]
    }
  }
}
```

---

## Crate Structure

```
crates/
├── jcode-classifier/           # NEW
│   ├── src/
│   │   ├── lib.rs              # Public exports
│   │   ├── config.rs           # ClassifierConfig types
│   │   ├── input.rs            # ClassifierInput builder
│   │   ├── classifier.rs       # Classifier trait
│   │   ├── session_classifier.rs  # Uses session's Provider
│   │   ├── stages.rs           # Stage 1 & Stage 2 prompts
│   │   ├── trust_boundaries.rs # Trust boundary logic
│   │   ├── decision.rs         # Decision types
│   │   └── denial_tracker.rs   # Escalation tracking
│   └── Cargo.toml
```

---

## Implementation Phases

### Phase 1: Core Infrastructure
- [ ] Create `jcode-classifier` crate
- [ ] Define `ClassifierInput`, `ClassifierResult`, `Decision` types
- [ ] Define `ClassifierConfig`, `TrustBoundaries`, `BlockRules` types
- [ ] Implement `Classifier` trait

### Phase 2: Session Provider Implementation
- [ ] Implement `SessionProviderClassifier` using `Provider::complete()`
- [ ] Write Stage 1 prompt
- [ ] Write Stage 2 prompt
- [ ] Implement response parsing

### Phase 3: Trust & Block Logic
- [ ] Implement trust boundary checking
- [ ] Implement block rule matching
- [ ] Implement allow exception matching
- [ ] Implement denial tracker (escalation)

### Phase 4: Configuration & Integration
- [ ] Add JSON schema for classifier config
- [ ] Extend existing settings parsing
- [ ] Integrate with `SafetySystem`
- [ ] Add `request_permission_ai` method

### Phase 5: UI
- [ ] Add permission mode selector to TUI
- [ ] Show classifier decision in permission prompt
- [ ] Display trust boundary status

### Phase 6: Testing
- [ ] Unit tests for trust boundary matching
- [ ] Unit tests for block rule matching
- [ ] Integration tests with session Provider
- [ ] Real traffic evaluation

---

## What We DON'T Do

| ~~Feature~~ | Reason |
|-------------|--------|
| ~~Custom API key for classifier~~ | Reuse existing Provider auth |
| ~~Local model support (Ollama)~~ | Complicated setup, maintenance burden |
| ~~DB-specific pattern matching~~ | Classifier handles everything holistically |
| ~~New auth flows~~ | Zero new auth surface |
| ~~Hard-coded model selection~~ | User configures via existing model selection |

---

## Security Considerations

### Prompt Injection Defense

The classifier operates on:
- **User messages** (what was requested)
- **Bare tool calls** (what will execute)

It deliberately excludes:
- Assistant messages (can't be talked into bad decisions)
- Tool outputs (primary injection vector)

### Reasoning-Blind Design

The classifier cannot be manipulated by the agent because:
1. Assistant messages are stripped from input
2. Stage 1 is a fast filter (minimal reasoning surface)
3. Stage 2 sees the agent's actions, not its justifications

### Denial Escalation

To prevent a compromised/overeager agent from retrying indefinitely:
- 3 consecutive denials → halt
- 20 total denials → halt
- User must manually intervene to continue

---

## Open Questions

1. **Stage 1 recall bias**: What default value balances safety vs. friction?
2. **Context window**: How many recent messages should Stage 2 include?
3. **Latency budget**: What's acceptable latency for the classifier?
4. **Fallback**: What happens if the classifier API call fails?

---

## References

- [Claude Code Auto Mode](https://www.anthropic.com/engineering/claude-code-auto-mode)
- [Claude Code Permissions Docs](https://code.claude.com/docs/en/permissions)
- jcode Provider trait (`jcode-provider-core/src/lib.rs`)
- jcode Safety System (`jcode-base/src/safety.rs`)
- jcode Command Risk (`jcode-command-risk/src/lib.rs`)
