//! Small built-in tools, useful for demos and tests.

use crate::tool::{Tool, ToolError};

/// Returns its input unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct Echo;

impl Tool for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Returns its input unchanged."
    }

    fn call(&self, input: &str) -> Result<String, ToolError> {
        Ok(input.to_owned())
    }
}

/// Sums whitespace-separated integers.
#[derive(Debug, Clone, Copy, Default)]
pub struct Add;

impl Tool for Add {
    fn name(&self) -> &'static str {
        "add"
    }

    fn description(&self) -> &'static str {
        "Sums whitespace-separated integers, e.g. `2 3 4` returns `9`."
    }

    fn call(&self, input: &str) -> Result<String, ToolError> {
        let mut total = 0_i64;
        let mut terms = 0_usize;

        for token in input.split_whitespace() {
            let value: i64 = token.parse().map_err(|err| {
                ToolError::InvalidInput(format!("`{token}` is not an integer ({err})"))
            })?;
            total = total
                .checked_add(value)
                .ok_or_else(|| ToolError::Failed("sum overflowed a 64-bit integer".to_owned()))?;
            terms += 1;
        }

        if terms == 0 {
            return Err(ToolError::InvalidInput(
                "expected at least one integer".to_owned(),
            ));
        }
        Ok(total.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_returns_input() {
        assert_eq!(Echo.call("  spaced  "), Ok("  spaced  ".to_owned()));
    }

    #[test]
    fn add_sums_including_negatives() {
        assert_eq!(Add.call("2 3 4"), Ok("9".to_owned()));
        assert_eq!(Add.call("-5   5 7"), Ok("7".to_owned()));
    }

    #[test]
    fn add_rejects_non_integers_and_empty_input() {
        assert!(matches!(Add.call("1 two"), Err(ToolError::InvalidInput(_))));
        assert!(matches!(Add.call("   "), Err(ToolError::InvalidInput(_))));
    }

    #[test]
    fn add_reports_overflow() {
        let input = format!("{} 1", i64::MAX);

        assert!(matches!(Add.call(&input), Err(ToolError::Failed(_))));
    }
}
