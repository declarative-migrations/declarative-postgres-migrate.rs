use dpm::diff::{Change, Plan};
use dpm::formal::{LeaseState, OwnerId};
use dpm::model::QName;
use dpm::plan_safety::{
    check_parallel_changes, BorrowMode, BorrowScope, ResourceBorrow, ResourcePath,
};
use proptest::prelude::*;

fn table(schema: &str, name: &str) -> QName {
    QName::new(schema, name)
}

#[test]
fn different_tables_share_a_wave_but_same_table_is_serialized() {
    let plan = Plan {
        changes: vec![
            Change::EnableRls {
                table: table("public", "users"),
            },
            Change::EnableRls {
                table: table("public", "orders"),
            },
            Change::ForceRls {
                table: table("public", "users"),
            },
        ],
    };

    let certificate = plan.borrow_check();
    certificate.validate().unwrap();
    assert_eq!(certificate.wave_count(), 2);
    assert_eq!(certificate.waves[0].steps.len(), 2);
    assert_eq!(certificate.waves[1].steps.len(), 1);
}

#[test]
fn schema_creation_is_a_barrier_for_objects_in_that_schema() {
    let create_schema = Change::CreateSchema {
        name: "tenant".to_string(),
    };
    let mutate_table = Change::EnableRls {
        table: table("tenant", "users"),
    };
    assert!(check_parallel_changes(&[&create_schema, &mutate_table]).is_err());
}

#[test]
fn certificate_is_deterministic_and_exact_plan_bound() {
    let left = Plan {
        changes: vec![Change::EnableRls {
            table: table("public", "users"),
        }],
    };
    let right = Plan {
        changes: vec![Change::EnableRls {
            table: table("public", "orders"),
        }],
    };

    let first = left.borrow_check();
    let second = left.borrow_check();
    assert_eq!(first, second);
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap()
    );
    assert!(first.validate_for(&left).is_ok());
    assert!(first.validate_for(&right).is_err());
}

#[test]
fn certified_plan_bridges_to_linear_typestate_and_lease_owner() {
    let plan = Plan {
        changes: vec![Change::EnableRls {
            table: table("public", "users"),
        }],
    };
    let migration = plan.certify().into_validated_migration();
    assert_eq!(migration.proof().checked_items(), 1);

    let mut leases = LeaseState::default();
    let guard = leases
        .acquire(OwnerId::new("planner-a").unwrap())
        .unwrap();
    let applied = migration.authorize(&guard).finish();
    let certified = applied.into_inner();
    assert_eq!(certified.certificate().change_count, 1);
    assert_eq!(certified.plan().changes.len(), 1);
    let receipt = guard.release();
    assert_eq!(receipt.owner().as_str(), "planner-a");
}

proptest! {
    #[test]
    fn borrow_conflicts_are_symmetric(
        left_segments in prop::collection::vec("[a-z]{1,8}", 1..5),
        right_segments in prop::collection::vec("[a-z]{1,8}", 1..5),
        left_exclusive in any::<bool>(),
        right_exclusive in any::<bool>(),
        left_subtree in any::<bool>(),
        right_subtree in any::<bool>(),
    ) {
        let left = ResourceBorrow {
            resource: ResourcePath::new(left_segments),
            mode: if left_exclusive {
                BorrowMode::Exclusive
            } else {
                BorrowMode::Shared
            },
            scope: if left_subtree {
                BorrowScope::Subtree
            } else {
                BorrowScope::Exact
            },
        };
        let right = ResourceBorrow {
            resource: ResourcePath::new(right_segments),
            mode: if right_exclusive {
                BorrowMode::Exclusive
            } else {
                BorrowMode::Shared
            },
            scope: if right_subtree {
                BorrowScope::Subtree
            } else {
                BorrowScope::Exact
            },
        };
        prop_assert_eq!(left.conflicts_with(&right), right.conflicts_with(&left));
    }

    #[test]
    fn every_generated_certificate_has_conflict_free_ordered_waves(
        operations in prop::collection::vec(
            (0u8..5, "[a-z]{1,6}", "[a-z]{1,6}"),
            0..128,
        )
    ) {
        let changes = operations
            .into_iter()
            .map(|(kind, schema, name)| match kind {
                0 => Change::CreateSchema { name: schema },
                1 => Change::DropSchema { name: schema },
                2 => Change::EnableRls { table: table(&schema, &name) },
                3 => Change::DisableRls { table: table(&schema, &name) },
                _ => Change::DropTable { table: table(&schema, &name) },
            })
            .collect();
        let plan = Plan { changes };
        let certificate = plan.borrow_check();
        prop_assert!(certificate.validate_for(&plan).is_ok());
    }
}
