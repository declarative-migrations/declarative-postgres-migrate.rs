//! Live-database introspection: build a [`Catalog`] from `pg_catalog`.
//!
//! Design rules:
//! - Every deparsed definition is produced by the server with
//!   `search_path = ''`, so object references are fully qualified and two
//!   databases with identical schemas produce byte-identical strings.
//! - PostgreSQL uses one batched query per object class. CockroachDB uses the
//!   same shape except for triggers, whose stable API requires per-table
//!   `SHOW TRIGGERS` / `SHOW CREATE TRIGGER` calls.
//! - Objects that belong to an extension (`pg_depend.deptype = 'e'`) are
//!   excluded: the extension itself is the diffable unit, not its internals.
//! - Works on Postgres 12+ (generated columns are the newest feature relied
//!   on; on older servers they are simply absent). Supabase (PG 15/17) is a
//!   first-class target: connect through a direct or session-mode connection,
//!   not the transaction pooler (dpm verifies `search_path` actually applied
//!   and fails loudly otherwise).
//! - CockroachDB is detected over pgwire. Where its PostgreSQL compatibility
//!   catalog has a gap (notably trigger deparsing), the equivalent stable
//!   `SHOW` surface is used instead.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use sqlx::postgres::PgConnection;
use sqlx::{Connection, Row};

use crate::model::*;

/// Schemas that are never diffed unless explicitly requested via
/// `--schemas`. Covers Postgres internals, common managed-service surfaces
/// (Supabase, pgbouncer), and popular extension-managed schemas.
pub const DEFAULT_EXCLUDED_SCHEMAS: &[&str] = &[
    // Postgres internals
    "information_schema",
    // Supabase-managed
    "auth",
    "storage",
    "realtime",
    "_realtime",
    "supabase_functions",
    "supabase_migrations",
    "graphql",
    "graphql_public",
    "pgbouncer",
    "pgsodium",
    "pgsodium_masks",
    "vault",
    "extensions",
    "pgtle",
    "_analytics",
    // extension-managed
    "net",
    "cron",
    "pgmq",
    "topology",
    "tiger",
    "tiger_data",
];

#[derive(Default)]
pub struct IntrospectOptions {
    /// Explicit schema list. When `None`, all non-system schemas minus
    /// [`DEFAULT_EXCLUDED_SCHEMAS`] and `extra_excluded` are used.
    pub schemas: Option<Vec<String>>,
    pub extra_excluded: Vec<String>,
}


pub async fn connect(url: &str) -> Result<PgConnection> {
    let mut conn = PgConnection::connect(url)
        .await
        .with_context(|| format!("failed to connect to {}", redact_url(url)))?;
    sqlx::raw_sql("SET search_path = ''")
        .execute(&mut conn)
        .await
        .context("failed to SET search_path")?;
    // Transaction-mode poolers (e.g. Supabase's pgbouncer on :6543) do not
    // preserve session state, which would silently break canonical deparsing.
    // Detect and refuse rather than produce garbage diffs.
    let sp: String = sqlx::query_scalar("SHOW search_path")
        .fetch_one(&mut conn)
        .await
        .context("failed to read back search_path")?;
    let normalized = sp.replace('"', "").trim().to_string();
    if !(normalized.is_empty() || normalized == "''") {
        bail!(
            "search_path did not stick (got {sp:?}); you are likely connected through a \
             transaction-mode pooler. Use a direct or session-mode connection for \
             introspection (Supabase: port 5432 / session pooler, not 6543)."
        );
    }
    Ok(conn)
}

/// Strip password from a URL for error messages.
pub fn redact_url(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => match rest.split_once('@') {
            Some((creds, host)) => {
                let user = creds.split(':').next().unwrap_or("");
                format!("{scheme}://{user}:***@{host}")
            }
            None => url.to_string(),
        },
        None => url.to_string(),
    }
}

pub async fn introspect_url(url: &str, opts: &IntrospectOptions) -> Result<Catalog> {
    let mut conn = connect(url).await?;
    let cat = introspect(&mut conn, opts).await;
    let _ = conn.close().await;
    cat
}

pub async fn introspect(conn: &mut PgConnection, opts: &IntrospectOptions) -> Result<Catalog> {
    let database_flavor = detect_database_flavor(conn).await?;
    let cockroach_database = if database_flavor == DatabaseFlavor::Cockroach {
        Some({
            let database: String = sqlx::query_scalar("SELECT current_database()")
                .fetch_one(&mut *conn)
                .await
                .context("reading CockroachDB database name")?;
            database
        })
    } else {
        None
    };
    // PostgreSQL exposes this setting as int4 while CockroachDB exposes its
    // PostgreSQL-compatibility value as int8.  Decode the wider type and
    // narrow only after the query so both pgwire servers can be inspected.
    let server_version_num: i64 = sqlx::query_scalar("SELECT current_setting('server_version_num')::bigint")
        .fetch_one(&mut *conn)
        .await
        .context("reading server_version_num")?;

    let schemas = resolve_schemas(conn, opts, database_flavor).await?;
    let schema_vec: Vec<String> = schemas.iter().cloned().collect();

    let mut cat = Catalog {
        format_version: CATALOG_FORMAT_VERSION,
        server_version_num: i32::try_from(server_version_num)
            .context("server_version_num does not fit in a signed 32-bit integer")?,
        database_flavor,
        schemas,
        ..Catalog::default()
    };

    load_extensions(conn, &mut cat).await.context("introspecting extensions")?;
    load_enums(conn, &schema_vec, &mut cat).await.context("introspecting enums")?;
    load_tables(conn, &schema_vec, &mut cat).await.context("introspecting tables")?;
    load_columns(conn, &schema_vec, &mut cat).await.context("introspecting columns")?;
    load_constraints(conn, &schema_vec, &mut cat)
        .await
        .context("introspecting constraints")?;
    load_indexes(conn, &schema_vec, &mut cat, cockroach_database.as_deref())
        .await
        .context("introspecting indexes")?;
    load_policies(conn, &schema_vec, &mut cat).await.context("introspecting policies")?;
    load_sequences(conn, &schema_vec, &mut cat)
        .await
        .context("introspecting sequences")?;
    load_views(conn, &schema_vec, &mut cat, cockroach_database.as_deref())
        .await
        .context("introspecting views")?;
    load_functions(conn, &schema_vec, &mut cat, cockroach_database.as_deref())
        .await
        .context("introspecting functions and procedures")?;
    load_triggers(conn, &schema_vec, &mut cat, cockroach_database.as_deref())
        .await
        .context("introspecting triggers")?;

    Ok(cat)
}

/// Identify the server using its version banner.  This works before any
/// catalog-specific query and is stable across PostgreSQL-wire clients.
pub async fn detect_database_flavor(conn: &mut PgConnection) -> Result<DatabaseFlavor> {
    let version: String = sqlx::query_scalar("SELECT version()")
        .fetch_one(&mut *conn)
        .await
        .context("reading database version")?;
    Ok(if version.to_ascii_lowercase().contains("cockroachdb") {
        DatabaseFlavor::Cockroach
    } else {
        DatabaseFlavor::Postgres
    })
}

async fn resolve_schemas(
    conn: &mut PgConnection,
    opts: &IntrospectOptions,
    database_flavor: DatabaseFlavor,
) -> Result<BTreeSet<String>> {
    if let Some(explicit) = &opts.schemas {
        return Ok(explicit.iter().cloned().collect());
    }
    let rows = sqlx::query(
        "SELECT nspname FROM pg_catalog.pg_namespace \
         WHERE nspname NOT LIKE 'pg\\_%' ESCAPE '\\' \
           AND nspname <> 'information_schema' \
           AND NOT EXISTS ( \
             SELECT 1 FROM pg_catalog.pg_depend d \
             WHERE d.classid = 'pg_namespace'::regclass \
               AND d.objid = pg_namespace.oid AND d.deptype = 'e')",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut out = BTreeSet::new();
    for row in rows {
        let name: String = row.get("nspname");
        let excluded = DEFAULT_EXCLUDED_SCHEMAS.contains(&name.as_str())
            || (database_flavor == DatabaseFlavor::Cockroach && name.starts_with("crdb_"))
            || opts.extra_excluded.iter().any(|e| e == &name);
        if !excluded {
            out.insert(name);
        }
    }
    Ok(out)
}

/// `NOT EXISTS` guard reused by every object query: skip anything installed
/// by an extension.
const NOT_EXTENSION_OWNED: &str = "NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend ext_d \
     WHERE ext_d.classid = $CLASS$::regclass AND ext_d.objid = $OID$ AND ext_d.deptype = 'e')";

fn not_ext(class: &str, oid_expr: &str) -> String {
    NOT_EXTENSION_OWNED.replace("$CLASS$", &format!("'{class}'")).replace("$OID$", oid_expr)
}

async fn load_extensions(conn: &mut PgConnection, cat: &mut Catalog) -> Result<()> {
    let rows = sqlx::query("SELECT extname FROM pg_catalog.pg_extension WHERE extname <> 'plpgsql'")
        .fetch_all(&mut *conn)
        .await?;
    for row in rows {
        cat.extensions.insert(row.get::<String, _>("extname"));
    }
    Ok(())
}

async fn load_enums(conn: &mut PgConnection, schemas: &[String], cat: &mut Catalog) -> Result<()> {
    let sql = format!(
        "SELECT n.nspname AS schema, t.typname AS name, \
                array_agg(e.enumlabel ORDER BY e.enumsortorder) AS labels \
         FROM pg_catalog.pg_type t \
         JOIN pg_catalog.pg_enum e ON e.enumtypid = t.oid \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
         WHERE n.nspname = ANY($1) AND {} \
         GROUP BY 1, 2",
        not_ext("pg_type", "t.oid")
    );
    for row in sqlx::query(&sql).bind(schemas).fetch_all(&mut *conn).await? {
        cat.enums.insert(
            QName::new(row.get::<String, _>("schema"), row.get::<String, _>("name")),
            row.get::<Vec<String>, _>("labels"),
        );
    }
    Ok(())
}

async fn load_tables(conn: &mut PgConnection, schemas: &[String], cat: &mut Catalog) -> Result<()> {
    let sql = format!(
        "SELECT n.nspname AS schema, c.relname AS name, \
                c.relrowsecurity AS rls, c.relforcerowsecurity AS rls_forced, \
                CASE WHEN c.relkind = 'p' THEN pg_get_partkeydef(c.oid) END AS partition_by \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind IN ('r', 'p') AND NOT coalesce(c.relispartition, false) \
           AND n.nspname = ANY($1) AND {}",
        not_ext("pg_class", "c.oid")
    );
    for row in sqlx::query(&sql).bind(schemas).fetch_all(&mut *conn).await? {
        cat.tables.insert(
            QName::new(row.get::<String, _>("schema"), row.get::<String, _>("name")),
            Table {
                columns: Vec::new(),
                constraints: BTreeMap::new(),
                indexes: BTreeMap::new(),
                partition_by: row.get::<Option<String>, _>("partition_by"),
                rls_enabled: row.get::<bool, _>("rls"),
                rls_forced: row.get::<bool, _>("rls_forced"),
                policies: BTreeMap::new(),
            },
        );
    }
    Ok(())
}

use std::collections::BTreeMap;

async fn load_columns(conn: &mut PgConnection, schemas: &[String], cat: &mut Catalog) -> Result<()> {
    // attgenerated exists from PG 12; guarded via server_version_num.
    let generated_expr = if cat.server_version_num >= 120000 { "a.attgenerated::text" } else { "''" };
    let (hidden_expr, visibility_join) = if cat.database_flavor == DatabaseFlavor::Cockroach {
        (
            "coalesce(isc.is_hidden = 'YES', false)",
            "LEFT JOIN information_schema.columns isc \
               ON isc.table_schema = n.nspname AND isc.table_name = c.relname \
              AND isc.column_name = a.attname",
        )
    } else {
        ("false", "")
    };
    let sql = format!(
        "SELECT n.nspname AS schema, c.relname AS tbl, a.attname AS name, \
                pg_catalog.format_type(a.atttypid, a.atttypmod) AS type_sql, \
                a.attnotnull AS not_null, a.attidentity::text AS identity, \
                {generated_expr} AS generated, \
                pg_catalog.pg_get_expr(ad.adbin, ad.adrelid) AS default_expr, \
                CASE WHEN a.attcollation <> t.typcollation THEN \
                  pg_catalog.quote_ident(cn.nspname) || '.' || pg_catalog.quote_ident(col.collname) \
                END AS collation, \
                {hidden_expr} AS hidden, \
                pg_catalog.pg_get_serial_sequence( \
                  pg_catalog.quote_ident(n.nspname) || '.' || pg_catalog.quote_ident(c.relname), \
                  a.attname) AS serial_seq \
         FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_catalog.pg_type t ON t.oid = a.atttypid \
         LEFT JOIN pg_catalog.pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum \
         LEFT JOIN pg_catalog.pg_collation col ON col.oid = a.attcollation \
         LEFT JOIN pg_catalog.pg_namespace cn ON cn.oid = col.collnamespace \
         {visibility_join} \
         WHERE c.relkind IN ('r', 'p') AND NOT coalesce(c.relispartition, false) \
           AND a.attnum > 0 AND NOT a.attisdropped \
           AND n.nspname = ANY($1) \
         ORDER BY n.nspname, c.relname, a.attnum"
    );
    for row in sqlx::query(&sql).bind(schemas).fetch_all(&mut *conn).await? {
        let q = QName::new(row.get::<String, _>("schema"), row.get::<String, _>("tbl"));
        let Some(table) = cat.tables.get_mut(&q) else { continue };
        let identity = match row.get::<String, _>("identity").as_str() {
            "a" => Some(IdentityKind::Always),
            "d" => Some(IdentityKind::ByDefault),
            _ => None,
        };
        let generated_kind: String = row.get("generated");
        let default_expr: Option<String> = row.get("default_expr");
        let serial_seq: Option<String> = row.get("serial_seq");
        let is_serial = identity.is_none()
            && serial_seq.is_some()
            && default_expr.as_deref().is_some_and(|d| d.starts_with("nextval("));
        let (generated, default) = if generated_kind == "s" {
            (default_expr, None)
        } else {
            (None, default_expr)
        };
        table.columns.push(Column {
            name: row.get("name"),
            type_sql: row.get("type_sql"),
            not_null: row.get("not_null"),
            default,
            identity,
            generated,
            is_serial,
            collation: row.get("collation"),
            hidden: row.get("hidden"),
        });
    }
    Ok(())
}

async fn load_constraints(conn: &mut PgConnection, schemas: &[String], cat: &mut Catalog) -> Result<()> {
    let sql = "SELECT n.nspname AS schema, c.relname AS tbl, con.conname AS name, \
                con.contype::text AS kind, pg_catalog.pg_get_constraintdef(con.oid) AS def \
         FROM pg_catalog.pg_constraint con \
         JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE con.contype IN ('p', 'u', 'c', 'f', 'x') \
           AND n.nspname = ANY($1)"
        .to_string();
    for row in sqlx::query(&sql).bind(schemas).fetch_all(&mut *conn).await? {
        let q = QName::new(row.get::<String, _>("schema"), row.get::<String, _>("tbl"));
        let Some(table) = cat.tables.get_mut(&q) else { continue };
        let Some(kind) = ConstraintKind::from_contype(&row.get::<String, _>("kind")) else { continue };
        let name: String = row.get("name");
        table.constraints.insert(
            name.clone(),
            Constraint { name, kind, def: row.get("def") },
        );
    }
    Ok(())
}

async fn load_indexes(
    conn: &mut PgConnection,
    schemas: &[String],
    cat: &mut Catalog,
    cockroach_database: Option<&str>,
) -> Result<()> {
    // Constraint-backed indexes (pk/unique/exclusion) are represented by
    // their constraint; only free-standing indexes are tracked here.
    let sql = "SELECT n.nspname AS schema, c.relname AS tbl, ic.relname AS name, \
                i.indisunique AS uniq, pg_catalog.pg_get_indexdef(i.indexrelid) AS def \
         FROM pg_catalog.pg_index i \
         JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
         JOIN pg_catalog.pg_class c ON c.oid = i.indrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = ANY($1) \
           AND NOT i.indisprimary \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con WHERE con.conindid = i.indexrelid)"
        .to_string();
    for row in sqlx::query(&sql).bind(schemas).fetch_all(&mut *conn).await? {
        let q = QName::new(row.get::<String, _>("schema"), row.get::<String, _>("tbl"));
        let Some(table) = cat.tables.get_mut(&q) else { continue };
        let name: String = row.get("name");
        let def: String = row.get("def");
        table.indexes.insert(
            name.clone(),
            Index {
                name,
                def: cockroach_database
                    .map(|database| strip_cockroach_database_qualifiers(&def, database))
                    .unwrap_or(def),
                unique: row.get("uniq"),
            },
        );
    }
    Ok(())
}

async fn load_policies(conn: &mut PgConnection, schemas: &[String], cat: &mut Catalog) -> Result<()> {
    let sql = "SELECT schemaname AS schema, tablename AS tbl, policyname AS name, \
                permissive = 'PERMISSIVE' AS permissive, cmd, \
                coalesce(roles::text[], '{}') AS roles, qual, with_check \
         FROM pg_catalog.pg_policies WHERE schemaname = ANY($1)"
        .to_string();
    for row in sqlx::query(&sql).bind(schemas).fetch_all(&mut *conn).await? {
        let q = QName::new(row.get::<String, _>("schema"), row.get::<String, _>("tbl"));
        let Some(table) = cat.tables.get_mut(&q) else { continue };
        let name: String = row.get("name");
        table.policies.insert(
            name.clone(),
            Policy {
                name,
                permissive: row.get("permissive"),
                command: row.get::<Option<String>, _>("cmd").unwrap_or_else(|| "ALL".into()),
                roles: row.get("roles"),
                using_expr: row.get("qual"),
                check_expr: row.get("with_check"),
            },
        );
    }
    Ok(())
}

async fn load_sequences(conn: &mut PgConnection, schemas: &[String], cat: &mut Catalog) -> Result<()> {
    // Owned sequences (serial columns / identity) are excluded via pg_depend
    // deptype 'a' (auto) / 'i' (internal).
    let sql = format!(
        "SELECT n.nspname AS schema, c.relname AS name, \
                pg_catalog.format_type(s.seqtypid, NULL) AS type_sql, \
                s.seqstart, s.seqincrement, s.seqmin, s.seqmax, s.seqcache, s.seqcycle \
         FROM pg_catalog.pg_sequence s \
         JOIN pg_catalog.pg_class c ON c.oid = s.seqrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = ANY($1) AND {} \
           AND NOT EXISTS ( \
             SELECT 1 FROM pg_catalog.pg_depend d \
             WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid \
               AND d.deptype IN ('a', 'i'))",
        not_ext("pg_class", "c.oid")
    );
    for row in sqlx::query(&sql).bind(schemas).fetch_all(&mut *conn).await? {
        cat.sequences.insert(
            QName::new(row.get::<String, _>("schema"), row.get::<String, _>("name")),
            Sequence {
                type_sql: row.get("type_sql"),
                start: row.get("seqstart"),
                increment: row.get("seqincrement"),
                min_value: row.get("seqmin"),
                max_value: row.get("seqmax"),
                cache: row.get("seqcache"),
                cycle: row.get("seqcycle"),
            },
        );
    }
    Ok(())
}

async fn load_views(
    conn: &mut PgConnection,
    schemas: &[String],
    cat: &mut Catalog,
    cockroach_database: Option<&str>,
) -> Result<()> {
    let sql = format!(
        "SELECT n.nspname AS schema, c.relname AS name, c.relkind = 'm' AS materialized, \
                pg_catalog.pg_get_viewdef(c.oid, true) AS def \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind IN ('v', 'm') AND n.nspname = ANY($1) AND {}",
        not_ext("pg_class", "c.oid")
    );
    for row in sqlx::query(&sql).bind(schemas).fetch_all(&mut *conn).await? {
        let def: String = row.get("def");
        cat.views.insert(
            QName::new(row.get::<String, _>("schema"), row.get::<String, _>("name")),
            View {
                materialized: row.get("materialized"),
                def: cockroach_database
                    .map(|database| strip_cockroach_database_qualifiers(&def, database))
                    .unwrap_or(def),
            },
        );
    }
    Ok(())
}

/// CockroachDB's deparsers qualify relation references with the *current
/// database* (`db.public.table`).  DPM compares different databases and
/// replays SQL into a fresh shadow database, so that ephemeral component must
/// not become part of the catalog.  This small lexer only removes matching
/// identifier tokens outside string literals; quoted identifiers and escaped
/// quotes are preserved correctly.
fn strip_cockroach_database_qualifiers(sql: &str, database: &str) -> String {
    fn is_ident_start(b: u8) -> bool {
        b.is_ascii_alphabetic() || b == b'_'
    }
    fn is_ident_continue(b: u8) -> bool {
        is_ident_start(b) || b.is_ascii_digit() || b == b'$'
    }
    fn dot_after(sql: &[u8], mut at: usize) -> Option<usize> {
        while sql.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        (sql.get(at) == Some(&b'.')).then_some(at)
    }

    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        i += 1;
                        if bytes.get(i) == Some(&b'\'') {
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                out.push_str(&sql[start..i]);
            }
            b'"' => {
                let start = i;
                i += 1;
                let mut ident = String::new();
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        i += 1;
                        if bytes.get(i) == Some(&b'"') {
                            ident.push('"');
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        let ch = sql[i..].chars().next().expect("valid UTF-8 input");
                        ident.push(ch);
                        i += ch.len_utf8();
                    }
                }
                if ident == database {
                    if let Some(dot) = dot_after(bytes, i) {
                        i = dot + 1;
                        continue;
                    }
                }
                out.push_str(&sql[start..i]);
            }
            b if is_ident_start(b) => {
                let start = i;
                i += 1;
                while bytes.get(i).is_some_and(|b| is_ident_continue(*b)) {
                    i += 1;
                }
                if &sql[start..i] == database {
                    if let Some(dot) = dot_after(bytes, i) {
                        i = dot + 1;
                        continue;
                    }
                }
                out.push_str(&sql[start..i]);
            }
            _ => {
                let ch = sql[i..].chars().next().expect("valid UTF-8 input");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

async fn load_functions(
    conn: &mut PgConnection,
    schemas: &[String],
    cat: &mut Catalog,
    cockroach_database: Option<&str>,
) -> Result<()> {
    // prokind: f = function, p = procedure. Aggregates/window functions can't
    // be deparsed by pg_get_functiondef and are skipped.
    let sql = format!(
        "SELECT p.oid, n.nspname AS schema, p.proname AS name, p.prokind::text AS kind, \
                pg_catalog.pg_get_function_identity_arguments(p.oid) AS ident_args, \
                pg_catalog.pg_get_functiondef(p.oid) AS def \
         FROM pg_catalog.pg_proc p \
         JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
         WHERE p.prokind IN ('f', 'p') AND n.nspname = ANY($1) AND {} \
         ORDER BY n.nspname, p.proname, p.oid",
        not_ext("pg_proc", "p.oid")
    );
    let rows = sqlx::query(&sql).bind(schemas).fetch_all(&mut *conn).await?;
    let mut cockroach_procedures = std::collections::BTreeMap::<QName, Vec<String>>::new();
    for row in &rows {
        if cockroach_database.is_some() && row.get::<String, _>("kind") == "p" {
            let q = QName::new(row.get::<String, _>("schema"), row.get::<String, _>("name"));
            if let std::collections::btree_map::Entry::Vacant(entry) =
                cockroach_procedures.entry(q.clone())
            {
                let show = format!("SHOW CREATE PROCEDURE {}", q.sql());
                let defs = sqlx::query(&show)
                    .fetch_all(&mut *conn)
                    .await
                    .with_context(|| format!("deparsing CockroachDB procedure {}", q.label()))?
                    .into_iter()
                    .map(|row| row.get::<String, _>("create_statement"))
                    .collect();
                entry.insert(defs);
            }
        }
    }
    let mut procedure_offsets = std::collections::BTreeMap::<QName, usize>::new();
    for row in rows {
        let schema: String = row.get("schema");
        let name: String = row.get("name");
        let kind = RoutineKind::from_prokind(&row.get::<String, _>("kind"))
            .expect("query restricts prokind to functions and procedures");
        let ident_args: String = row.get("ident_args");
        let signature = format!("{name}({ident_args})");
        let key = format!("{schema}.{signature}");
        let q = QName::new(schema, name.clone());
        let def = if cockroach_database.is_some() && kind == RoutineKind::Procedure {
            let offset = procedure_offsets.entry(q.clone()).or_default();
            let definitions = cockroach_procedures
                .get(&q)
                .expect("procedure definitions were loaded above");
            let def = definitions.get(*offset).cloned().with_context(|| {
                format!(
                    "CockroachDB returned fewer SHOW CREATE definitions than pg_proc rows for {}",
                    q.label()
                )
            })?;
            *offset += 1;
            def
        } else {
            row.get("def")
        };
        let def = cockroach_database
            .map(|database| strip_cockroach_database_qualifiers(&def, database))
            .unwrap_or(def);
        cat.functions.insert(
            key,
            Function {
                signature,
                name,
                identity_arguments: ident_args,
                kind,
                def,
            },
        );
    }
    Ok(())
}

async fn load_triggers(
    conn: &mut PgConnection,
    schemas: &[String],
    cat: &mut Catalog,
    cockroach_database: Option<&str>,
) -> Result<()> {
    if let Some(database) = cockroach_database {
        return load_cockroach_triggers(conn, cat, database).await;
    }
    let sql = "SELECT n.nspname AS schema, c.relname AS tbl, t.tgname AS name, \
                t.tgenabled::text AS mode, \
                pg_catalog.pg_get_triggerdef(t.oid) AS def \
         FROM pg_catalog.pg_trigger t \
         JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE NOT t.tgisinternal AND n.nspname = ANY($1)"
        .to_string();
    for row in sqlx::query(&sql).bind(schemas).fetch_all(&mut *conn).await? {
        let schema: String = row.get("schema");
        let tbl: String = row.get("tbl");
        let name: String = row.get("name");
        let key = format!("{schema}.{tbl}.{name}");
        cat.triggers.insert(
            key,
            Trigger {
                table: QName::new(schema, tbl),
                name,
                mode: TriggerMode::from_postgres(&row.get::<String, _>("mode"))
                    .expect("PostgreSQL returned an unknown trigger mode"),
                def: row.get("def"),
            },
        );
    }
    Ok(())
}

/// CockroachDB v25.2 supports row-level triggers but does not populate
/// `pg_trigger` or implement `pg_get_triggerdef`. Its documented introspection
/// API is `SHOW TRIGGERS FROM table` plus `SHOW CREATE TRIGGER ... ON table`.
async fn load_cockroach_triggers(
    conn: &mut PgConnection,
    cat: &mut Catalog,
    database: &str,
) -> Result<()> {
    let tables: Vec<QName> = cat.tables.keys().cloned().collect();
    for table in tables {
        let show = format!("SHOW TRIGGERS FROM {}", table.sql());
        for row in sqlx::query(&show)
            .fetch_all(&mut *conn)
            .await
            .with_context(|| format!("listing CockroachDB triggers on {}", table.label()))?
        {
            let name: String = row.get("trigger_name");
            let enabled: bool = row.get("enabled");
            let show_create = format!(
                "SHOW CREATE TRIGGER {} ON {}",
                quote_ident(&name),
                table.sql()
            );
            let def: String = sqlx::query(&show_create)
                .fetch_one(&mut *conn)
                .await
                .with_context(|| {
                    format!(
                        "deparsing CockroachDB trigger {}.{}",
                        table.label(),
                        name
                    )
                })?
                .get("create_statement");
            let key = format!("{}.{}.{}", table.schema, table.name, name);
            cat.triggers.insert(
                key,
                Trigger {
                    table: table.clone(),
                    name,
                    mode: if enabled {
                        TriggerMode::Origin
                    } else {
                        TriggerMode::Disabled
                    },
                    def: strip_cockroach_database_qualifiers(&def, database),
                },
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cockroach_database_qualifiers_are_canonicalized_without_touching_literals() {
        let sql = "SELECT dpm_source.public.users.id, 'dpm_source.public.users', \"dpm_source\".app.\"Audit\" FROM dpm_source.public.users";
        assert_eq!(
            strip_cockroach_database_qualifiers(sql, "dpm_source"),
            "SELECT public.users.id, 'dpm_source.public.users', app.\"Audit\" FROM public.users"
        );
    }

    #[test]
    fn cockroach_database_qualifier_normalization_preserves_unicode_identifiers() {
        let sql = "SELECT \"dépôt\".public.\"café\", 'café'";
        assert_eq!(
            strip_cockroach_database_qualifiers(sql, "dépôt"),
            "SELECT public.\"café\", 'café'"
        );
    }
}
