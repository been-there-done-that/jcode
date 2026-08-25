//! Denial tracking for escalation logic.

use std::time::{Duration, Instant};

/// Tracks denials for escalation logic.
///
/// When the classifier blocks too many actions in a row or too many total,
/// we escalate to the user rather than letting the agent retry indefinitely.
#[derive(Debug, Clone)]
pub struct DenialTracker {
    /// Number of consecutive denials.
    consecutive: usize,

    /// Total number of denials.
    total: usize,

    /// When the consecutive streak started.
    consecutive_since: Option<Instant>,

    /// Maximum consecutive denials before escalation.
    max_consecutive: usize,

    /// Maximum total denials before escalation.
    max_total: usize,

    /// Time limit for consecutive denials (resets streak if exceeded).
    consecutive_timeout: Duration,
}

impl DenialTracker {
    /// Create a new denial tracker with default limits.
    pub fn new() -> Self {
        Self::with_limits(
            3,                                    // max_consecutive
            20,                                   // max_total
            Duration::from_secs(60),              // consecutive_timeout
        )
    }

    /// Create a denial tracker with custom limits.
    pub fn with_limits(
        max_consecutive: usize,
        max_total: usize,
        consecutive_timeout: Duration,
    ) -> Self {
        Self {
            consecutive: 0,
            total: 0,
            consecutive_since: None,
            max_consecutive,
            max_total,
            consecutive_timeout,
        }
    }

    /// Record a denial and determine the escalation action.
    pub fn record(&mut self, denied: bool) -> EscalationAction {
        if denied {
            self.total += 1;

            // Track consecutive
            let now = Instant::now();
            if let Some(since) = self.consecutive_since {
                if now.duration_since(since) > self.consecutive_timeout {
                    // Reset streak
                    self.consecutive = 1;
                    self.consecutive_since = Some(now);
                } else {
                    self.consecutive += 1;
                }
            } else {
                self.consecutive = 1;
                self.consecutive_since = Some(now);
            }

            // Check escalation conditions
            if self.consecutive >= self.max_consecutive {
                EscalationAction::HaltAndPromptUser {
                    reason: EscalationReason::TooManyConsecutive(self.consecutive),
                }
            } else if self.total >= self.max_total {
                EscalationAction::HaltAndPromptUser {
                    reason: EscalationReason::TooManyTotal(self.total),
                }
            } else {
                EscalationAction::Continue
            }
        } else {
            // Reset on any non-denial
            self.consecutive = 0;
            self.consecutive_since = None;
            EscalationAction::Continue
        }
    }

    /// Reset the tracker.
    pub fn reset(&mut self) {
        self.consecutive = 0;
        self.total = 0;
        self.consecutive_since = None;
    }

    /// Get current stats.
    pub fn stats(&self) -> DenialStats {
        DenialStats {
            consecutive: self.consecutive,
            total: self.total,
        }
    }
}

impl Default for DenialTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Stats about denials.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenialStats {
    pub consecutive: usize,
    pub total: usize,
}

/// Action to take after a denial.
#[derive(Debug, Clone)]
pub enum EscalationAction {
    /// Continue normally.
    Continue,
    /// Halt and prompt the user.
    HaltAndPromptUser {
        reason: EscalationReason,
    },
}

/// Reason for escalation.
#[derive(Debug, Clone)]
pub enum EscalationReason {
    TooManyConsecutive(usize),
    TooManyTotal(usize),
}

impl std::fmt::Display for EscalationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EscalationReason::TooManyConsecutive(n) => {
                write!(f, "Too many consecutive denials ({})", n)
            }
            EscalationReason::TooManyTotal(n) => {
                write!(f, "Too many total denials ({})", n)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consecutive_escalation() {
        let mut tracker = DenialTracker::with_limits(3, 20, Duration::from_secs(60));
        
        assert!(matches!(tracker.record(true), EscalationAction::Continue));
        assert!(matches!(tracker.record(true), EscalationAction::Continue));
        assert!(matches!(
            tracker.record(true),
            EscalationAction::HaltAndPromptUser { .. }
        ));
    }

    #[test]
    fn test_total_escalation() {
        let mut tracker = DenialTracker::with_limits(3, 5, Duration::from_secs(60));
        
        for _ in 0..4 {
            tracker.record(true);
        }
        assert!(matches!(tracker.record(true), EscalationAction::HaltAndPromptUser { .. }));
    }

    #[test]
    fn test_reset_on_allow() {
        let mut tracker = DenialTracker::with_limits(3, 20, Duration::from_secs(60));
        
        tracker.record(true);
        tracker.record(true);
        tracker.record(false); // Reset
        
        assert!(matches!(tracker.record(true), EscalationAction::Continue));
        assert!(matches!(tracker.record(true), EscalationAction::Continue));
        assert!(matches!(
            tracker.record(true),
            EscalationAction::HaltAndPromptUser { .. }
        ));
    }
}
