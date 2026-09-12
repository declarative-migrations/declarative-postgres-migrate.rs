//! Advisory (non-DDL) analysis of the desired catalog. Ported from the
//! pg-defs `foreignKeyIndexRecommendations` house tooling: every foreign key
//! should have an index whose leading column is the FK's leading referencing
//! column — without one, cascading deletes scan the child table and child
//! writes contend with the parent.
//!
//! Advisories are emitted as comments only; they never change the migration
//! semantics (creating them live would make the target diverge from the
//! source on the next diff).

use crate::model::*;

#[derive(Debug, Clone)]
pub struct FkIndexAdvice {
    pub table: QName,
    pub constraint: String,
    pub column: String,
    pub suggested_statement: String,
}

pub fn advise_fk_indexes(cat: &Catalog) -> Vec<FkIndexAdvice> {
    cat.tables
        .iter()
        .flat_map(|(q, table)| {
            let covered: Vec<String> = table
                .constraints
                .values()
                .filter(|con| {
                    matches!(
                        con.kind,
                        ConstraintKind::PrimaryKey | ConstraintKind::Unique
                    )
                })
                .filter_map(|con| leading_column_of_key_list(&con.def))
                .chain(
                    table
                        .indexes
                        .values()
                        .filter_map(|idx| leading_column_of_indexdef(&idx.def)),
                )
                .collect();

            table.constraints.values().filter_map(move |con| {
                (con.kind == ConstraintKind::ForeignKey)
                    .then(|| leading_column_of_key_list(&con.def))
                    .flatten()
                    .filter(|col| !covered.iter().any(|c| c.eq_ignore_ascii_case(col)))
                    .map(|col| {
                        let index_name = format!("{}_{}_idx", q.name, col.replace('"', ""));
                        FkIndexAdvice {
                            table: q.clone(),
                            constraint: con.name.clone(),
                            column: col.clone(),
                            suggested_statement: format!(
                                "CREATE INDEX IF NOT EXISTS {} ON {} ({});",
                                quote_ident(&index_name),
                                q.sql(),
                                quote_ident(&col)
                            ),
                        }
                    })
            })
        })
        .collect()
}

pub fn advisory_comment_block(advice: &[FkIndexAdvice]) -> String {
    match advice {
        [] => String::new(),
        advice => {
            let header = format!(
                "-- =============================================================\n\
                 -- Advisory: {} foreign key(s) without a supporting index\n\
                 -- Derived from the desired schema; statements are suggestions only\n\
                 -- and are NOT part of the migration (add them to the source of\n\
                 -- truth if you want them).\n\
                 -- =============================================================\n",
                advice.len()
            );
            let body: String = advice
                .iter()
                .map(|a| {
                    format!(
                        "-- {}.{} ({}):\n--   {}\n",
                        a.table.label(),
                        a.column,
                        a.constraint,
                        a.suggested_statement
                    )
                })
                .collect();
            header + &body
        }
    }
}

/// Leading column from `... KEY (a, b) ...` / `FOREIGN KEY (a) REFERENCES ...`
/// / `PRIMARY KEY (a, b)` / `UNIQUE (a)` deparsed definitions.
fn leading_column_of_key_list(def: &str) -> Option<String> {
    let open = def.find('(')?;
    let rest = &def[open + 1..];
    let end = rest.find([',', ')'])?;
    let raw = rest[..end].trim();
    (!raw.is_empty()).then(|| raw.trim_matches('"').to_string())
}

/// Leading column from a full `pg_get_indexdef` statement:
/// `CREATE [UNIQUE] INDEX name ON schema.tbl USING btree (col, ...)`.
/// Expression indexes (leading token contains '(' or a function call) return
/// the raw expression text, which simply never matches a bare column name.
fn leading_column_of_indexdef(def: &str) -> Option<String> {
    let using = def.find(" USING ")?;
    let after = &def[using + 7..];
    let open = after.find('(')?;
    let rest = &after[open + 1..];
    let end = match rest
        .char_indices()
        .try_fold(0usize, |depth, (i, ch)| match (ch, depth) {
            ('(', d) => Ok(d + 1),
            (')', d) if d > 0 => Ok(d - 1),
            (')' | ',', 0) => Err(i),
            (_, d) => Ok(d),
        }) {
        Ok(_) => rest.len(),
        Err(i) => i,
    };
    let raw = rest[..end].trim();
    (!raw.is_empty()).then(|| raw.trim_matches('"').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn table() -> Table {
        Table {
            columns: Vec::new(),
            constraints: BTreeMap::new(),
            indexes: BTreeMap::new(),
            partition_by: None,
            rls_enabled: false,
            rls_forced: false,
            policies: BTreeMap::new(),
        }
    }

    #[test]
    fn parses_leading_columns() {
        assert_eq!(
            leading_column_of_key_list("FOREIGN KEY (user_id) REFERENCES public.users(id)"),
            Some("user_id".to_string())
        );
        assert_eq!(
            leading_column_of_key_list("PRIMARY KEY (id, org)"),
            Some("id".to_string())
        );
        assert_eq!(
            leading_column_of_indexdef(
                "CREATE INDEX t_idx ON public.t USING btree (org_id, created_at)"
            ),
            Some("org_id".to_string())
        );
        assert_eq!(
            leading_column_of_indexdef(
                "CREATE INDEX t_expr ON public.t USING btree (lower(email))"
            ),
            Some("lower(email)".to_string())
        );
    }

    #[test]
    fn uncovered_fk_is_advised_and_covered_fk_is_not() {
        let mut cat = Catalog::empty_with_schemas(["public".into()]);
        let q = QName::new("public", "orders");
        let mut t = table();
        t.constraints.insert(
            "orders_user_fkey".into(),
            Constraint {
                name: "orders_user_fkey".into(),
                kind: ConstraintKind::ForeignKey,
                def: "FOREIGN KEY (user_id) REFERENCES public.users(id)".into(),
            },
        );
        t.constraints.insert(
            "orders_org_fkey".into(),
            Constraint {
                name: "orders_org_fkey".into(),
                kind: ConstraintKind::ForeignKey,
                def: "FOREIGN KEY (org_id) REFERENCES public.orgs(id)".into(),
            },
        );
        t.indexes.insert(
            "orders_org_idx".into(),
            Index {
                name: "orders_org_idx".into(),
                def: "CREATE INDEX orders_org_idx ON public.orders USING btree (org_id)".into(),
                unique: false,
            },
        );
        cat.tables.insert(q, t);
        let advice = advise_fk_indexes(&cat);
        assert_eq!(advice.len(), 1);
        assert_eq!(advice[0].column, "user_id");
        assert!(advice[0].suggested_statement.contains("orders_user_id_idx"));
    }
}
