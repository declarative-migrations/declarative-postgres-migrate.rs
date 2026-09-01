//! Hierarchical typed-plan resources and shared/exclusive borrow semantics.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::Serialize;

use crate::diff::Change;
use crate::model::QName;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowMode {
    Shared,
    Exclusive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowScope {
    Exact,
    Subtree,
}

/// Stable, serializable identity for a database object or object family.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ResourcePath {
    segments: Vec<String>,
}

impl ResourcePath {
    pub fn new<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let segments = segments.into_iter().map(Into::into).collect::<Vec<_>>();
        assert!(!segments.is_empty(), "a resource path cannot be empty");
        Self { segments }
    }

    pub fn database() -> Self {
        Self::new(["database"])
    }

    pub fn schema(name: &str) -> Self {
        Self::new(["database", "schema", name])
    }

    pub fn object(schema: &str, kind: &str, name: &str) -> Self {
        Self::new(["database", "schema", schema, kind, name])
    }

    pub fn table(table: &QName) -> Self {
        Self::object(&table.schema, "table", &table.name)
    }

    pub fn as_segments(&self) -> &[String] {
        &self.segments
    }

    fn is_prefix_of(&self, other: &Self) -> bool {
        self.segments.len() <= other.segments.len()
            && self
                .segments
                .iter()
                .zip(other.segments.iter())
                .all(|(left, right)| left == right)
    }
}

impl fmt::Display for ResourcePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.segments.join("/"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ResourceBorrow {
    pub resource: ResourcePath,
    pub mode: BorrowMode,
    pub scope: BorrowScope,
}

impl ResourceBorrow {
    pub fn shared(resource: ResourcePath) -> Self {
        Self {
            resource,
            mode: BorrowMode::Shared,
            scope: BorrowScope::Exact,
        }
    }

    pub fn exclusive(resource: ResourcePath) -> Self {
        Self {
            resource,
            mode: BorrowMode::Exclusive,
            scope: BorrowScope::Exact,
        }
    }

    pub fn shared_subtree(resource: ResourcePath) -> Self {
        Self {
            resource,
            mode: BorrowMode::Shared,
            scope: BorrowScope::Subtree,
        }
    }

    pub fn exclusive_subtree(resource: ResourcePath) -> Self {
        Self {
            resource,
            mode: BorrowMode::Exclusive,
            scope: BorrowScope::Subtree,
        }
    }

    pub fn conflicts_with(&self, other: &Self) -> bool {
        if self.mode == BorrowMode::Shared && other.mode == BorrowMode::Shared {
            return false;
        }
        if self.resource == other.resource {
            return true;
        }
        (self.scope == BorrowScope::Subtree && self.resource.is_prefix_of(&other.resource))
            || (other.scope == BorrowScope::Subtree && other.resource.is_prefix_of(&self.resource))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BorrowedStep {
    pub index: usize,
    pub operation: &'static str,
    pub destructive: bool,
    pub manual: bool,
    pub borrows: Vec<ResourceBorrow>,
}

impl BorrowedStep {
    pub fn from_change(index: usize, change: &Change) -> Self {
        let borrows = change_borrows(change).into_iter().collect();
        Self {
            index,
            operation: change_operation(change),
            destructive: change.is_destructive(),
            manual: change.is_manual(),
            borrows,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BorrowConflict {
    pub left_step: usize,
    pub right_step: usize,
    pub left: ResourceBorrow,
    pub right: ResourceBorrow,
}

impl fmt::Display for BorrowConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "migration steps {} and {} conflict: {:?} {} ({:?}) vs {:?} {} ({:?})",
            self.left_step,
            self.right_step,
            self.left.mode,
            self.left.resource,
            self.left.scope,
            self.right.mode,
            self.right.resource,
            self.right.scope
        )
    }
}

impl Error for BorrowConflict {}

pub(crate) fn step_conflicts_with_any(step: &BorrowedStep, others: &[BorrowedStep]) -> bool {
    others
        .iter()
        .any(|other| first_conflict(step, other).is_some())
}

pub(crate) fn first_conflict<'a>(
    left: &'a BorrowedStep,
    right: &'a BorrowedStep,
) -> Option<(&'a ResourceBorrow, &'a ResourceBorrow)> {
    left.borrows.iter().find_map(|left_borrow| {
        right
            .borrows
            .iter()
            .find(|right_borrow| left_borrow.conflicts_with(right_borrow))
            .map(|right_borrow| (left_borrow, right_borrow))
    })
}

fn change_borrows(change: &Change) -> BTreeSet<ResourceBorrow> {
    let mut borrows = BTreeSet::new();

    macro_rules! shared_subtree {
        ($resource:expr) => {{
            borrows.insert(ResourceBorrow::shared_subtree($resource));
        }};
    }
    macro_rules! exclusive {
        ($resource:expr) => {{
            borrows.insert(ResourceBorrow::exclusive($resource));
        }};
    }
    macro_rules! exclusive_subtree {
        ($resource:expr) => {{
            borrows.insert(ResourceBorrow::exclusive_subtree($resource));
        }};
    }

    match change {
        Change::CreateSchema { name } | Change::DropSchema { name } => {
            exclusive_subtree!(ResourcePath::schema(name));
        }
        Change::CreateExtension { .. }
        | Change::MoveExtension { .. }
        | Change::DropExtension { .. } => {
            // Extensions can install objects in multiple schemas.
            exclusive_subtree!(ResourcePath::database());
        }
        Change::CreateEnum { ty, .. }
        | Change::AddEnumValue { ty, .. }
        | Change::EnumNeedsRebuild { ty, .. }
        | Change::DropEnum { ty } => {
            exclusive!(ResourcePath::object(&ty.schema, "enum", &ty.name));
        }
        Change::CreateSequence { seq, .. }
        | Change::AlterSequence { seq, .. }
        | Change::DropSequence { seq } => {
            exclusive!(ResourcePath::object(&seq.schema, "sequence", &seq.name));
        }
        Change::CreateTable { table, .. }
        | Change::DropTable { table }
        | Change::AddColumn { table, .. }
        | Change::DropColumn { table, .. }
        | Change::AlterColumnType { table, .. }
        | Change::SetNotNull { table, .. }
        | Change::DropNotNull { table, .. }
        | Change::SetColumnVisibility { table, .. }
        | Change::SetDefault { table, .. }
        | Change::DropDefault { table, .. }
        | Change::MakeSerial { table, .. }
        | Change::DropSerial { table, .. }
        | Change::AddIdentity { table, .. }
        | Change::DropIdentity { table, .. }
        | Change::SetIdentityKind { table, .. }
        | Change::RegenerateColumn { table, .. }
        | Change::AddConstraint { table, .. }
        | Change::DropConstraint { table, .. }
        | Change::CreateIndex { table, .. }
        | Change::EnableRls { table }
        | Change::DisableRls { table }
        | Change::ForceRls { table }
        | Change::UnforceRls { table }
        | Change::CreatePolicy { table, .. }
        | Change::DropPolicy { table, .. }
        | Change::CreateTrigger { table, .. }
        | Change::DropTrigger { table, .. }
        | Change::SetTriggerMode { table, .. } => {
            // PostgreSQL DDL takes relation-level locks; serialize all direct
            // mutations of the same table, even when columns differ.
            exclusive_subtree!(ResourcePath::table(table));
        }
        Change::DropIndex { index, .. } => {
            // The plan does not retain the parent table for a dropped index.
            // Borrow the schema subtree conservatively rather than guess.
            exclusive_subtree!(ResourcePath::schema(&index.schema));
        }
        Change::CreateView { view, .. } | Change::DropView { view, .. } => {
            exclusive!(ResourcePath::object(&view.schema, "view", &view.name));
        }
        Change::CreateFunction { key, .. } => {
            let schema = key
                .split_once('.')
                .map(|(schema, _)| schema)
                .unwrap_or("<unknown>");
            exclusive!(ResourcePath::object(schema, "routine", key));
        }
        Change::DropFunction { key, schema, .. } => {
            exclusive!(ResourcePath::object(schema, "routine", key));
        }
    }

    // Definitions and expressions can reference objects outside the directly
    // mutated resource. A shared database-subtree borrow makes those opaque
    // dependencies conflict with every concurrent exclusive mutation without
    // preventing two read-only dependency sets from coexisting.
    if has_opaque_dependencies(change) {
        shared_subtree!(ResourcePath::database());
    }

    borrows
}

fn has_opaque_dependencies(change: &Change) -> bool {
    match change {
        Change::CreateTable { .. }
        | Change::DropTable { .. }
        | Change::AddColumn { .. }
        | Change::DropColumn { .. }
        | Change::AlterColumnType { .. }
        | Change::SetDefault { .. }
        | Change::DropDefault { .. }
        | Change::MakeSerial { .. }
        | Change::DropSerial { .. }
        | Change::RegenerateColumn { .. }
        | Change::AddConstraint { .. }
        | Change::DropConstraint { .. }
        | Change::CreateIndex { .. }
        | Change::DropIndex { .. }
        | Change::CreatePolicy { .. }
        | Change::DropPolicy { .. }
        | Change::CreateView { .. }
        | Change::DropView { .. }
        | Change::CreateFunction { .. }
        | Change::DropFunction { .. }
        | Change::CreateTrigger { .. }
        | Change::DropTrigger { .. } => true,
        Change::CreateSchema { .. }
        | Change::DropSchema { .. }
        | Change::CreateExtension { .. }
        | Change::MoveExtension { .. }
        | Change::DropExtension { .. }
        | Change::CreateEnum { .. }
        | Change::AddEnumValue { .. }
        | Change::EnumNeedsRebuild { .. }
        | Change::DropEnum { .. }
        | Change::CreateSequence { .. }
        | Change::AlterSequence { .. }
        | Change::DropSequence { .. }
        | Change::SetNotNull { .. }
        | Change::DropNotNull { .. }
        | Change::SetColumnVisibility { .. }
        | Change::AddIdentity { .. }
        | Change::DropIdentity { .. }
        | Change::SetIdentityKind { .. }
        | Change::EnableRls { .. }
        | Change::DisableRls { .. }
        | Change::ForceRls { .. }
        | Change::UnforceRls { .. }
        | Change::SetTriggerMode { .. } => false,
    }
}

fn change_operation(change: &Change) -> &'static str {
    match change {
        Change::CreateSchema { .. } => "create_schema",
        Change::DropSchema { .. } => "drop_schema",
        Change::CreateExtension { .. } => "create_extension",
        Change::MoveExtension { .. } => "move_extension",
        Change::DropExtension { .. } => "drop_extension",
        Change::CreateEnum { .. } => "create_enum",
        Change::AddEnumValue { .. } => "add_enum_value",
        Change::EnumNeedsRebuild { .. } => "enum_needs_rebuild",
        Change::DropEnum { .. } => "drop_enum",
        Change::CreateSequence { .. } => "create_sequence",
        Change::AlterSequence { .. } => "alter_sequence",
        Change::DropSequence { .. } => "drop_sequence",
        Change::CreateTable { .. } => "create_table",
        Change::DropTable { .. } => "drop_table",
        Change::AddColumn { .. } => "add_column",
        Change::DropColumn { .. } => "drop_column",
        Change::AlterColumnType { .. } => "alter_column_type",
        Change::SetNotNull { .. } => "set_not_null",
        Change::DropNotNull { .. } => "drop_not_null",
        Change::SetColumnVisibility { .. } => "set_column_visibility",
        Change::SetDefault { .. } => "set_default",
        Change::DropDefault { .. } => "drop_default",
        Change::MakeSerial { .. } => "make_serial",
        Change::DropSerial { .. } => "drop_serial",
        Change::AddIdentity { .. } => "add_identity",
        Change::DropIdentity { .. } => "drop_identity",
        Change::SetIdentityKind { .. } => "set_identity_kind",
        Change::RegenerateColumn { .. } => "regenerate_column",
        Change::AddConstraint { .. } => "add_constraint",
        Change::DropConstraint { .. } => "drop_constraint",
        Change::CreateIndex { .. } => "create_index",
        Change::DropIndex { .. } => "drop_index",
        Change::EnableRls { .. } => "enable_rls",
        Change::DisableRls { .. } => "disable_rls",
        Change::ForceRls { .. } => "force_rls",
        Change::UnforceRls { .. } => "unforce_rls",
        Change::CreatePolicy { .. } => "create_policy",
        Change::DropPolicy { .. } => "drop_policy",
        Change::CreateView { .. } => "create_view",
        Change::DropView { .. } => "drop_view",
        Change::CreateFunction { .. } => "create_function",
        Change::DropFunction { .. } => "drop_function",
        Change::CreateTrigger { .. } => "create_trigger",
        Change::DropTrigger { .. } => "drop_trigger",
        Change::SetTriggerMode { .. } => "set_trigger_mode",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_borrows_can_coexist() {
        let resource = ResourcePath::object("public", "table", "users");
        assert!(!ResourceBorrow::shared(resource.clone())
            .conflicts_with(&ResourceBorrow::shared(resource)));
    }

    #[test]
    fn exclusive_borrow_conflicts_with_shared_borrow() {
        let resource = ResourcePath::object("public", "table", "users");
        assert!(ResourceBorrow::exclusive(resource.clone())
            .conflicts_with(&ResourceBorrow::shared(resource)));
    }

    #[test]
    fn schema_subtree_conflicts_with_child_table() {
        let schema = ResourceBorrow::exclusive_subtree(ResourcePath::schema("public"));
        let table = ResourceBorrow::exclusive(ResourcePath::object("public", "table", "users"));
        assert!(schema.conflicts_with(&table));
        assert!(table.conflicts_with(&schema));
    }

    #[test]
    fn sibling_tables_do_not_conflict() {
        let left =
            ResourceBorrow::exclusive_subtree(ResourcePath::object("public", "table", "users"));
        let right =
            ResourceBorrow::exclusive_subtree(ResourcePath::object("public", "table", "orders"));
        assert!(!left.conflicts_with(&right));
    }
}
