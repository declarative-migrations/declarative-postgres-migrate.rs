//! Canonicalize deparsed CHECK-constraint text through the server itself.
//!
//! `pg_get_constraintdef` output is not always a re-parse fixed point. The
//! canonical example is a varchar IN-list CHECK: Postgres stores the original
//! parse as `(col)::text = ANY ((ARRAY['a'::character varying, ...])::text[])`,
//! but feeding that emitted text back through the parser stores per-element
//! casts (`ANY (ARRAY[('a'::character varying)::text, ...])`), which deparses
//! differently. Raw string comparison therefore reports an eternal diff
//! between a freshly-parsed schema and a database built from dpm's own
//! emitted SQL — the same constraint is dropped and re-added forever.
//!
//! The fix stays true to the project's core idea (the server is the only
//! trustworthy normalizer — never regex): every CHECK def is rebuilt once on
//! a scratch table in a throwaway shadow database and the re-read deparse is
//! substituted into the catalog. One round-trip is the fixed point: an
//! already-canonical def re-canonicalizes to itself, so both sides of any
//! diff land on identical strings regardless of how their databases were
//! built. Defs that fail to rebuild (extension-owned types missing on the
//! shadow, etc.) are left untouched — degraded, never wrong: the worst case
//! is the pre-existing behavior.
//!
//! Only CHECK constraints are rewritten today. PK/unique/FK/exclusion defs
//! are column-list shaped and round-trip stably; `pg_get_indexdef` has not
//! shown a non-fixed-point shape in the matrix fixtures. If one surfaces,
//! this module is where its canonicalization belongs.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use sqlx::Connection;

use crate::introspect;
use crate::model::{quote_ident, quote_literal, Catalog, ConstraintKind, DatabaseFlavor, Table};
use crate::source::ShadowDb;

/// Rewrite every CHECK constraint def in `catalogs` to its server-canonical
/// (re-parse fixed point) form, using a throwaway database on
/// `shadow_server_url`. PostgreSQL only; a no-op when no catalog carries a
/// CHECK constraint. Identical (column-signature, def) pairs across catalogs
/// are round-tripped once and shared.
pub async fn canonicalize_checks(
    catalogs: &mut [&mut Catalog],
    shadow_server_url: &str,
    verbose: bool,
) -> Result<()> {
    let has_checks = catalogs.iter().any(|c| {
        c.tables
            .values()
            .any(|t| t.constraints.values().any(|k| k.kind == ConstraintKind::Check))
    });
    if !has_checks {
        return Ok(());
    }
    // CockroachDB catalogs deparse through SHOW CREATE and have their own
    // normalization rules; scratch-table DDL below is PostgreSQL-shaped.
    if catalogs.iter().any(|c| c.database_flavor != DatabaseFlavor::Postgres) {
        return Ok(());
    }

    let scratch = ShadowDb::create(shadow_server_url, verbose).await?;
    if scratch.database_flavor() != DatabaseFlavor::Postgres {
        scratch.drop_db().await;
        return Ok(());
    }
    let result = canonicalize_on(&scratch, catalogs, verbose).await;
    scratch.drop_db().await;
    result
}

async fn canonicalize_on(
    scratch: &ShadowDb,
    catalogs: &mut [&mut Catalog],
    verbose: bool,
) -> Result<()> {
    let mut conn = sqlx::postgres::PgConnection::connect(&scratch.url)
        .await
        .with_context(|| {
            format!(
                "connecting to canonicalization scratch database {}",
                introspect::redact_url(&scratch.url)
            )
        })?;

    // Best-effort environment: user schemas, extensions, and enum types give
    // column definitions something to resolve against. Failures are fine —
    // any def they break simply stays un-canonicalized.
    for catalog in catalogs.iter() {
        for schema in &catalog.schemas {
            let _ = sqlx::raw_sql(&format!("CREATE SCHEMA IF NOT EXISTS {}", quote_ident(schema)))
                .execute(&mut conn)
                .await;
        }
        for ext in &catalog.extensions {
            let _ = sqlx::raw_sql(&format!("CREATE EXTENSION IF NOT EXISTS {}", quote_ident(ext)))
                .execute(&mut conn)
                .await;
        }
        for (qname, labels) in &catalog.enums {
            let labels_sql =
                labels.iter().map(|l| quote_literal(l)).collect::<Vec<_>>().join(", ");
            let _ = sqlx::raw_sql(&format!(
                "CREATE TYPE {}.{} AS ENUM ({labels_sql})",
                quote_ident(&qname.schema),
                quote_ident(&qname.name)
            ))
            .execute(&mut conn)
            .await;
        }
    }

    // (column-signature, def) → canonical def. The signature keys the scratch
    // table an expression was parsed against; the same def under the same
    // columns canonicalizes identically across catalogs.
    let mut canonical: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut scratch_tables: BTreeMap<String, String> = BTreeMap::new();

    for catalog in catalogs.iter_mut() {
        for table in catalog.tables.values_mut() {
            let sig = column_signature(table);
            let mut defs: Vec<String> = table
                .constraints
                .values()
                .filter(|c| c.kind == ConstraintKind::Check)
                .map(|c| c.def.clone())
                .collect();
            defs.retain(|d| !canonical.contains_key(&(sig.clone(), d.clone())));
            defs.sort();
            defs.dedup();

            if !defs.is_empty() {
                let scratch_table = match scratch_tables.get(&sig) {
                    Some(t) => t.clone(),
                    None => {
                        let name = format!("_dpm_canon_{}", scratch_tables.len());
                        let cols = table
                            .columns
                            .iter()
                            .map(|c| {
                                let mut d = format!("{} {}", quote_ident(&c.name), c.type_sql);
                                if let Some(coll) = &c.collation {
                                    d.push_str(&format!(" COLLATE {coll}"));
                                }
                                d
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        let create =
                            format!("CREATE TABLE public.{} ({cols})", quote_ident(&name));
                        match sqlx::raw_sql(&create).execute(&mut conn).await {
                            Ok(_) => {
                                scratch_tables.insert(sig.clone(), name.clone());
                                name
                            }
                            Err(e) => {
                                if verbose {
                                    eprintln!(
                                        "dpm: canonicalize: scratch table for signature failed \
                                         ({e}); leaving {} def(s) as-is",
                                        defs.len()
                                    );
                                }
                                for d in defs {
                                    canonical.insert((sig.clone(), d.clone()), d);
                                }
                                continue;
                            }
                        }
                    }
                };

                for def in defs {
                    let round_tripped =
                        round_trip_def(&mut conn, &scratch_table, &def).await;
                    let canon = match round_tripped {
                        Ok(c) => c,
                        Err(e) => {
                            if verbose {
                                eprintln!(
                                    "dpm: canonicalize: CHECK def did not round-trip ({e}); \
                                     comparing it verbatim: {def}"
                                );
                            }
                            def.clone()
                        }
                    };
                    canonical.insert((sig.clone(), def), canon);
                }
            }

            for con in table.constraints.values_mut() {
                if con.kind == ConstraintKind::Check {
                    if let Some(canon) = canonical.get(&(sig.clone(), con.def.clone())) {
                        con.def = canon.clone();
                    }
                }
            }
        }
    }
    let _ = conn.close().await;
    Ok(())
}

/// Add the def to the scratch table, read back the server's deparse, drop it.
async fn round_trip_def(
    conn: &mut sqlx::postgres::PgConnection,
    scratch_table: &str,
    def: &str,
) -> Result<String> {
    let add = format!(
        "ALTER TABLE public.{} ADD CONSTRAINT _dpm_canon_check {def}",
        quote_ident(scratch_table)
    );
    sqlx::raw_sql(&add).execute(&mut *conn).await.context("ADD CONSTRAINT failed")?;
    let row: (String,) = sqlx::query_as(
        "SELECT pg_catalog.pg_get_constraintdef(oid) FROM pg_catalog.pg_constraint \
         WHERE conname = '_dpm_canon_check' AND conrelid = ($1::text)::regclass",
    )
    .bind(format!("public.{}", quote_ident(scratch_table)))
    .fetch_one(&mut *conn)
    .await
    .context("reading back canonical constraint def")?;
    let drop = format!(
        "ALTER TABLE public.{} DROP CONSTRAINT _dpm_canon_check",
        quote_ident(scratch_table)
    );
    sqlx::raw_sql(&drop).execute(&mut *conn).await.context("DROP CONSTRAINT failed")?;
    Ok(row.0)
}

/// Identity of the column environment a CHECK expression parses against.
fn column_signature(table: &Table) -> String {
    let mut parts: Vec<String> = table
        .columns
        .iter()
        .map(|c| {
            format!("{}\u{1}{}\u{1}{}", c.name, c.type_sql, c.collation.as_deref().unwrap_or(""))
        })
        .collect();
    parts.sort();
    parts.join("\u{2}")
}

#[cfg(test)]
mod tests {
    use super::column_signature;
    use crate::model::{Column, Table};

    fn col(name: &str, type_sql: &str) -> Column {
        Column {
            name: name.into(),
            type_sql: type_sql.into(),
            not_null: false,
            default: None,
            identity: None,
            generated: None,
            is_serial: false,
            collation: None,
            hidden: false,
        }
    }

    #[test]
    fn signature_ignores_column_order_but_not_types() {
        let a = Table {
            columns: vec![col("x", "text"), col("y", "integer")],
            constraints: Default::default(),
            indexes: Default::default(),
            partition_by: None,
            rls_enabled: false,
            rls_forced: false,
            policies: Default::default(),
        };
        let mut b = a.clone();
        b.columns.reverse();
        assert_eq!(column_signature(&a), column_signature(&b));
        let mut c = a.clone();
        c.columns[0].type_sql = "character varying(32)".into();
        assert_ne!(column_signature(&a), column_signature(&c));
    }
}
