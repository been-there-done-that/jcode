//! Tests for the decision module.

use jcode_classifier::decision::{BlockCategory, Decision};

#[test]
fn test_decision_allow_display() {
    let decision = Decision::Allow;
    assert_eq!(format!("{}", decision), "ALLOW");
}

#[test]
fn test_decision_block_display() {
    let decision = Decision::Block {
        category: BlockCategory::DestroyExfiltrate,
        detail: "Deleted production database".to_string(),
    };
    let display = format!("{}", decision);
    assert!(display.contains("BLOCK"));
    assert!(display.contains("destroy/exfiltrate"));
    assert!(display.contains("Deleted production database"));
}

#[test]
fn test_decision_block_all_categories() {
    let categories = vec![
        (BlockCategory::DestroyExfiltrate, "destroy/exfiltrate"),
        (BlockCategory::DegradeSecurity, "degrade security"),
        (BlockCategory::CrossBoundaries, "cross boundaries"),
        (BlockCategory::BypassReview, "bypass review"),
    ];
    
    for (category, expected) in categories {
        let decision = Decision::Block {
            category,
            detail: "test detail".to_string(),
        };
        let display = format!("{}", decision);
        assert!(display.contains(expected), "Category {:?} not found in: {}", category, display);
    }
}

#[test]
fn test_decision_clone() {
    let decision = Decision::Block {
        category: BlockCategory::DegradeSecurity,
        detail: "Disable logging".to_string(),
    };
    let cloned = decision.clone();
    
    match (&decision, &cloned) {
        (Decision::Block { category: c1, detail: d1 }, Decision::Block { category: c2, detail: d2 }) => {
            assert_eq!(c1, c2);
            assert_eq!(d1, d2);
        }
        _ => panic!("Clone didn't match"),
    }
}

#[test]
fn test_decision_partial_eq() {
    let decision1 = Decision::Allow;
    let decision2 = Decision::Allow;
    assert_eq!(decision1, decision2);
    
    let decision3 = Decision::Block {
        category: BlockCategory::DestroyExfiltrate,
        detail: "test".to_string(),
    };
    let decision4 = Decision::Block {
        category: BlockCategory::DestroyExfiltrate,
        detail: "test".to_string(),
    };
    assert_eq!(decision3, decision4);
    
    // Different categories
    let decision5 = Decision::Block {
        category: BlockCategory::DestroyExfiltrate,
        detail: "test".to_string(),
    };
    let decision6 = Decision::Block {
        category: BlockCategory::DegradeSecurity,
        detail: "test".to_string(),
    };
    assert_ne!(decision5, decision6);
}
