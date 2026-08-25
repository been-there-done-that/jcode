# Auto Mode

AI-assisted permission classification for jcode sessions.

## Overview

Auto Mode uses an AI classifier to automatically approve or deny permission requests from the ambient agent, instead of always requiring manual user confirmation.

## Configuration

Add to `~/.jcode/config.toml`:

```toml
[permissions]
# Set to "auto" to enable AI-assisted permission decisions
# Default is "manual" (existing behavior)
default_mode = "auto"

[permissions.auto_mode]
# Model to use for classification. 
# - null/omitted: Use session's current model (default)
# - "claude": Use default Claude model
# - "claude:claude-sonnet-4-20250514": Use specific model
classifier_model = null

# Recall bias (0.0 = balanced, 1.0 = catch everything)
# Higher values block more actions (fewer false negatives)
recall_bias = 0.8

[permissions.auto_mode.trust_boundaries]
# The git repository you're working in (default: true)
working_directory = true

# Trusted git remote patterns (glob patterns)
git_remotes = []

# Internal services/domains
internal_services = []

[permissions.auto_mode.block_rules]
# Block permanent data destruction or exfiltration (default: true)
destroy_exfiltrate = true

# Block weakening system security (default: true)
degrade_security = true

# Block accessing untrusted infrastructure (default: true)
cross_boundaries = true

# Block skipping review safeguards (default: true)
bypass_review = true

# Exceptions: actions that match block rules but are still allowed
allow_exceptions = [
    "package_install_from_lockfile",
    "standard_credential_flows",
    "push_to_working_branch",
]
```

## How It Works

### Two-Stage Classification

1. **Stage 1 (Fast Filter)**: Lightweight evaluation returning YES/NO
   - Quick assessment based on action type and context
   - Default: blocks risky actions, allows safe ones

2. **Stage 2 (Full Reasoning)**: Thorough evaluation when Stage 1 flags an action
   - Detailed analysis of authorization, blast radius, recovery options
   - Used for borderline cases

### Decision Format

The classifier returns decisions in this format:
- `ALLOW`: Action is authorized and safe
- `BLOCK: <category> - <detail>`: Action blocked with reason

Categories:
- `destroy/exfiltrate`: Permanent data destruction or exfiltration
- `degrade security`: Weakening system security
- `cross boundaries`: Accessing untrusted infrastructure
- `bypass review`: Skipping review safeguards

### Fallback

If the classifier fails (e.g., API error), the system falls back to manual mode (queue for user review).

### Escalation

The denial tracker monitors consecutive denials. After 3 consecutive denials or 20 total denials, the system may escalate to require manual confirmation.

## Usage

1. Edit `~/.jcode/config.toml` with the settings above
2. Start a jcode session
3. When the ambient agent requests permission, the AI classifier will evaluate it
4. Approved actions proceed automatically
5. Denied actions return a denial message with the reason

## Example Output

**Approved:**
```
Permission auto-approved: This is a read-only directory listing within the working directory.
```

**Denied:**
```
Permission auto-denied: This would permanently delete all .log files in the project.
```

## Security Considerations

- The classifier uses the same Provider (API key/auth) as your session
- Trust boundaries should be configured for your environment
- The recall bias setting affects the balance between security and convenience
- Block rules provide defense-in-depth but shouldn't replace careful configuration
