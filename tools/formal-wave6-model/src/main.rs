use std::collections::BTreeSet;

const USERS: &str = "users";
const POSTS: &str = "posts";
const COMMENTS: &str = "comments";

fn dependencies(node: &str) -> &'static [&'static str] {
    match node {
        USERS => {
            return &[];
        }
        POSTS => {
            return &[USERS];
        }
        COMMENTS => {
            return &[POSTS];
        }
        _ => {
            panic!("unknown migration node: {node}");
        }
    }
}

fn valid_order(order: [&str; 3]) -> bool {
    let mut seen = BTreeSet::new();

    for node in order {
        if !dependencies(node)
            .iter()
            .all(|dependency| seen.contains(dependency))
        {
            return false;
        }

        seen.insert(node);
    }

    return true;
}

fn valid_orders() -> Vec<[&'static str; 3]> {
    let nodes = [USERS, POSTS, COMMENTS];
    let mut valid = Vec::new();

    for first in nodes {
        for second in nodes {
            if second == first {
                continue;
            }

            for third in nodes {
                if third == first || third == second {
                    continue;
                }

                let order = [first, second, third];
                if valid_order(order) {
                    valid.push(order);
                }
            }
        }
    }

    return valid;
}

fn duplicate_application_is_idempotent() -> bool {
    let mut applied = BTreeSet::from([USERS]);
    let before = applied.clone();
    applied.insert(USERS);

    return applied == before;
}

fn prove() {
    assert_eq!(valid_orders(), vec![[USERS, POSTS, COMMENTS]]);
    assert!(duplicate_application_is_idempotent());
    assert!(!valid_order([POSTS, USERS, COMMENTS]));
    assert!(!valid_order([USERS, COMMENTS, POSTS]));
}

fn main() {
    prove();
    println!("migration partial-order/idempotency model: ok");
    return;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_order_is_unique() {
        assert_eq!(valid_orders(), vec![[USERS, POSTS, COMMENTS]]);
    }

    #[test]
    fn duplicate_application_is_idempotent() {
        assert!(super::duplicate_application_is_idempotent());
    }

    #[test]
    fn invalid_orders_are_rejected() {
        assert!(!valid_order([POSTS, USERS, COMMENTS]));
        assert!(!valid_order([USERS, COMMENTS, POSTS]));
    }
}
