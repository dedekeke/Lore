use std::path::PathBuf;

use super::loader::EvalError;
use super::types::EvalCase;

/// Load the committed synthetic mini-fixture.
/// Looks up `eval/fixtures/mini.json` relative to `CARGO_MANIFEST_DIR`.
pub fn load_mini() -> Result<Vec<EvalCase>, EvalError> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("eval")
        .join("fixtures")
        .join("mini.json");
    let raw = std::fs::read_to_string(&path)?;
    let cases: Vec<EvalCase> = serde_json::from_str(&raw)?;
    Ok(cases)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::types::EvalEvent;

    #[test]
    fn mini_fixture_loads_and_validates() {
        let cases = load_mini().expect("mini fixture loads");
        assert!(!cases.is_empty(), "mini fixture should not be empty");
        for case in &cases {
            assert!(!case.id.is_empty(), "case id cannot be empty");
            assert!(
                !case.events.is_empty(),
                "case must have events: {}",
                case.id
            );
            // At least one scoring anchor — RecallRules or GetActiveContext.
            assert!(
                case.events.iter().any(|e| matches!(
                    e,
                    EvalEvent::RecallRules { .. } | EvalEvent::GetActiveContext { .. }
                )),
                "case {} must have a RecallRules or GetActiveContext event",
                case.id
            );
        }
    }
}
