//! Dependency-free operator failure diagnostic. Kept separately buildable so
//! SQLx upgrades cannot hide regressions in error reporting.
pub(crate) fn partial_failure_context(
    index: usize, total: usize, executed: usize, truncated_statement: &str,
) -> String {
    format!(
        "statement {index}/{total} failed after {executed} earlier successful execute calls; \
partial migration possible: attempted ROLLBACK cannot undo previously committed DDL; \
catalog restoration is UNVERIFIED. Re-introspect the target and generate a residual reconciliation plan before retrying.\n{truncated_statement}"
    )
}
#[cfg(test)]
mod tests {
    use super::partial_failure_context;
    #[test]
    fn autocommit_failure_never_claims_catalog_restoration() {
        let actual = partial_failure_context(2, 3, 1, "ALTER TABLE example ADD COLUMN x INT");
        for required in ["statement 2/3", "after 1 earlier", "catalog restoration is UNVERIFIED", "Re-introspect"] {
            assert!(actual.contains(required), "missing diagnostic: {required}");
        }
    }
    #[test]
    fn first_statement_failure_reports_zero_prior_successes() {
        let report=partial_failure_context(1,1,0,"bad SQL");
        assert!(report.contains("0 earlier successful"));
    }
}
