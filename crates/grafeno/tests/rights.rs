//! Phase 14.2: roles and privileges. A property test draws random role
//! graphs (acyclic), grants and denials on the server, namespaces, labels
//! and properties, and accounts with roles and grants, and checks every
//! right [`Rights`] computes against a direct model that walks the roles
//! for each question. Then: cycles are refused, deleting a role takes its
//! grants along, the rights come back through the database, and the
//! access generation moves with every change.

use std::collections::{HashMap, HashSet};

use grafeno::{
    Account, AccountKind, Act, Grafeno, Grant, Object, PUBLIC, Privilege, Rights, Role, RoleDef,
    Timestamp,
};
use proptest::prelude::*;

const PATTERNS: [&str; 5] = ["a", "b", "team-*", "*", "team-x"];
const NAMESPACES: [&str; 5] = ["a", "b", "team-x", "teamy", "other"];
const LABELS: [&str; 2] = ["Person", "Salary"];
const PROPERTIES: [&str; 2] = ["email", "name"];
const ROLES: [Role; 3] = [Role::Read, Role::Write, Role::Admin];

fn covers(pattern: &str, ns: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => ns.starts_with(prefix),
        None => pattern == ns,
    }
}

fn privilege() -> impl Strategy<Value = Privilege> {
    let deny = any::<bool>();
    prop_oneof![
        (0..Act::SERVER.len(), deny).prop_map(|(a, deny)| Privilege {
            action: Act::SERVER[a],
            object: Object::Server,
            deny,
        }),
        (0..Act::NAMESPACE.len(), 0..PATTERNS.len(), deny).prop_map(|(a, p, deny)| {
            Privilege {
                action: Act::NAMESPACE[a],
                object: Object::Namespace {
                    namespace: PATTERNS[p].into(),
                },
                deny,
            }
        }),
        (
            any::<bool>(),
            0..PATTERNS.len(),
            0..LABELS.len(),
            any::<bool>(),
            deny
        )
            .prop_map(|(write, p, l, edges, deny)| Privilege {
                action: if write { Act::Write } else { Act::Read },
                object: Object::Label {
                    namespace: PATTERNS[p].into(),
                    label: LABELS[l].into(),
                    edges
                },
                deny,
            }),
        (any::<bool>(), 0..PATTERNS.len(), 0..PROPERTIES.len(), deny).prop_map(
            |(write, p, q, deny)| Privilege {
                action: if write { Act::Write } else { Act::Read },
                object: Object::Property {
                    namespace: PATTERNS[p].into(),
                    label: "Person".into(),
                    property: PROPERTIES[q].into(),
                },
                deny,
            }
        ),
    ]
}

fn grant() -> impl Strategy<Value = Grant> {
    (0..PATTERNS.len(), 0..ROLES.len()).prop_map(|(p, r)| Grant {
        namespace: PATTERNS[p].into(),
        role: ROLES[r],
    })
}

/// Roles `r0..rN`; each may have the roles before it (so no cycle), and
/// maybe `public`.
fn world() -> impl Strategy<Value = (HashMap<String, RoleDef>, Account)> {
    (1usize..12, any::<bool>())
        .prop_flat_map(|(count, public)| {
            let roles = proptest::collection::vec(
                (
                    proptest::collection::vec(privilege(), 0..5),
                    proptest::collection::vec(grant(), 0..2),
                    proptest::collection::vec(any::<prop::sample::Index>(), 0..3),
                ),
                count,
            );
            let account = (
                proptest::collection::vec(any::<prop::sample::Index>(), 0..3),
                proptest::collection::vec(grant(), 0..2),
                prop::bool::weighted(0.05),
            );
            (roles, account, Just(public))
        })
        .prop_map(|(roles, (picks, grants, superuser), public)| {
            let mut defs = HashMap::new();
            for (i, (privileges, grants, parents)) in roles.iter().enumerate() {
                let name = if public && i == roles.len() - 1 {
                    PUBLIC.to_owned()
                } else {
                    format!("r{i}")
                };
                let below: Vec<String> = if i == 0 {
                    Vec::new()
                } else {
                    parents.iter().map(|p| format!("r{}", p.index(i))).collect()
                };
                defs.insert(
                    name.clone(),
                    RoleDef {
                        name,
                        privileges: privileges.clone(),
                        grants: grants.clone(),
                        roles: below,
                        policies: Vec::new(),
                        description: None,
                        created_at: Timestamp::from_micros(0),
                    },
                );
            }
            let names: Vec<String> = defs.keys().filter(|n| *n != PUBLIC).cloned().collect();
            let mut sorted = names.clone();
            sorted.sort();
            let account = Account {
                name: "acct".into(),
                kind: AccountKind::User,
                password_hash: None,
                grants,
                roles: if sorted.is_empty() {
                    Vec::new()
                } else {
                    picks
                        .iter()
                        .map(|p| sorted[p.index(sorted.len())].clone())
                        .collect()
                },
                superuser,
                own_only: false,
                disabled: false,
                expires_at: None,
                attributes: Default::default(),
                description: None,
                created_at: Timestamp::from_micros(0),
            };
            (defs, account)
        })
}

/// Every privilege and grant the account has, walking its roles plainly.
fn collect(account: &Account, roles: &HashMap<String, RoleDef>) -> (Vec<Privilege>, Vec<Grant>) {
    let mut privileges = Vec::new();
    let mut grants = account.grants.clone();
    let mut seen = HashSet::new();
    fn visit(
        name: &str,
        roles: &HashMap<String, RoleDef>,
        seen: &mut HashSet<String>,
        privileges: &mut Vec<Privilege>,
        grants: &mut Vec<Grant>,
    ) {
        if !seen.insert(name.to_owned()) {
            return;
        }
        let Some(role) = roles.get(name) else { return };
        privileges.extend(role.privileges.iter().cloned());
        grants.extend(role.grants.iter().cloned());
        for below in &role.roles {
            visit(below, roles, seen, privileges, grants);
        }
    }
    for name in &account.roles {
        visit(name, roles, &mut seen, &mut privileges, &mut grants);
    }
    if roles.contains_key(PUBLIC) {
        visit(PUBLIC, roles, &mut seen, &mut privileges, &mut grants);
    }
    (privileges, grants)
}

/// The model: allowed and denied, at the namespace level.
fn namespace_level(
    privileges: &[Privilege],
    grants: &[Grant],
    action: Act,
    ns: &str,
) -> (bool, bool) {
    let mut allow = grants
        .iter()
        .any(|g| covers(&g.namespace, ns) && grafeno::level_actions(g.role).contains(&action));
    let mut deny = false;
    for p in privileges {
        if let Object::Namespace { namespace } = &p.object
            && p.action == action
            && covers(namespace, ns)
        {
            if p.deny {
                deny = true;
            } else {
                allow = true;
            }
        }
    }
    (allow, deny)
}

fn model_server(account: &Account, privileges: &[Privilege], action: Act) -> bool {
    if account.superuser {
        return true;
    }
    let allow = privileges
        .iter()
        .any(|p| p.object == Object::Server && p.action == action && !p.deny);
    let deny = privileges
        .iter()
        .any(|p| p.object == Object::Server && p.action == action && p.deny);
    allow && !deny
}

fn model_namespace(
    account: &Account,
    privileges: &[Privilege],
    grants: &[Grant],
    action: Act,
    ns: &str,
) -> bool {
    if account.superuser {
        return true;
    }
    let (allow, deny) = namespace_level(privileges, grants, action, ns);
    (allow && !deny)
        || (!deny
            && action == Act::CreateNamespace
            && model_server(account, privileges, Act::CreateNamespace))
}

fn label_level(
    privileges: &[Privilege],
    grants: &[Grant],
    action: Act,
    ns: &str,
    label: &str,
    edges: bool,
) -> (bool, bool) {
    let level = if action == Act::Read {
        Act::Read
    } else {
        Act::Write
    };
    let (mut allow, mut deny) = namespace_level(privileges, grants, action, ns);
    for p in privileges {
        if let Object::Label {
            namespace,
            label: l,
            edges: e,
        } = &p.object
            && p.action == level
            && covers(namespace, ns)
            && l == label
            && *e == edges
        {
            if p.deny {
                deny = true;
            } else {
                allow = true;
            }
        }
    }
    (allow, deny)
}

fn model_label(
    account: &Account,
    privileges: &[Privilege],
    grants: &[Grant],
    action: Act,
    ns: &str,
    label: &str,
    edges: bool,
) -> bool {
    if account.superuser {
        return true;
    }
    let (allow, deny) = label_level(privileges, grants, action, ns, label, edges);
    allow && !deny
}

fn model_property(
    account: &Account,
    privileges: &[Privilege],
    grants: &[Grant],
    action: Act,
    ns: &str,
    property: &str,
) -> bool {
    if account.superuser {
        return true;
    }
    let level = if action == Act::Read {
        Act::Read
    } else {
        Act::Write
    };
    let (mut allow, mut deny) = label_level(privileges, grants, action, ns, "Person", false);
    for p in privileges {
        if let Object::Property {
            namespace,
            label,
            property: q,
        } = &p.object
            && p.action == level
            && covers(namespace, ns)
            && label == "Person"
            && q == property
        {
            if p.deny {
                deny = true;
            } else {
                allow = true;
            }
        }
    }
    allow && !deny
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn computed_rights_equal_the_model((roles, account) in world()) {
        let rights = Rights::compute(&account, &roles);
        let (privileges, grants) = collect(&account, &roles);
        for action in Act::SERVER {
            prop_assert_eq!(rights.on_server(action), model_server(&account, &privileges, action), "server {:?}", action);
        }
        for ns in NAMESPACES {
            for action in Act::NAMESPACE {
                prop_assert_eq!(
                    rights.on_namespace(action, ns),
                    model_namespace(&account, &privileges, &grants, action, ns),
                    "{:?} on {}", action, ns
                );
            }
            for action in [Act::Read, Act::Create, Act::Update, Act::Delete] {
                for label in LABELS {
                    for edges in [false, true] {
                        prop_assert_eq!(
                            rights.on_label(action, ns, label, edges),
                            model_label(&account, &privileges, &grants, action, ns, label, edges),
                            "{:?} on {}:{} (edges {})", action, ns, label, edges
                        );
                    }
                }
                for property in PROPERTIES {
                    prop_assert_eq!(
                        rights.on_property(action, ns, "Person", property),
                        model_property(&account, &privileges, &grants, action, ns, property),
                        "{:?} on {}:Person.{}", action, ns, property
                    );
                }
            }
        }
    }
}

fn role(name: &str, privileges: Vec<Privilege>, roles: &[&str]) -> RoleDef {
    RoleDef {
        name: name.into(),
        privileges,
        grants: Vec::new(),
        roles: roles.iter().map(|r| (*r).to_owned()).collect(),
        policies: Vec::new(),
        description: None,
        created_at: Timestamp::now(),
    }
}

#[test]
fn roles_in_the_database() {
    let db = Grafeno::in_memory().unwrap();
    let crm = |action| {
        Privilege::grant(
            action,
            Object::Namespace {
                namespace: "crm".into(),
            },
        )
    };
    let salary = Privilege::deny(
        Act::Read,
        Object::Label {
            namespace: "crm".into(),
            label: "Salary".into(),
            edges: false,
        },
    );
    let generation = |db: &Grafeno| db.begin_read().unwrap().access_generation().unwrap();
    let before = generation(&db);
    db.write(|tx| {
        tx.put_role(&role(
            "crm-reader",
            vec![crm(Act::Read), salary.clone()],
            &[],
        ))?;
        tx.put_role(&role(
            "crm-lead",
            vec![crm(Act::Create), crm(Act::Update)],
            &["crm-reader"],
        ))?;
        tx.create_account(&Account {
            name: "ana".into(),
            kind: AccountKind::User,
            password_hash: None,
            grants: Vec::new(),
            roles: vec!["crm-lead".into()],
            superuser: false,
            own_only: false,
            disabled: false,
            expires_at: None,
            attributes: Default::default(),
            description: None,
            created_at: Timestamp::now(),
        })
    })
    .unwrap();
    assert!(
        generation(&db) > before,
        "changes move the access generation"
    );
    let read = db.begin_read().unwrap();
    let ana = read.account("ana").unwrap().unwrap();
    let rights = read.rights(&ana).unwrap();
    // "Read everything in crm except Salary": one grant and one denial.
    assert!(rights.on_namespace(Act::Read, "crm") && rights.on_namespace(Act::Create, "crm"));
    assert!(!rights.on_namespace(Act::Delete, "crm"));
    assert!(rights.on_label(Act::Read, "crm", "Person", false));
    assert!(!rights.on_label(Act::Read, "crm", "Salary", false));
    assert!(rights.on_property(Act::Read, "crm", "Person", "email"));
    assert!(!rights.on_namespace(Act::Read, "other"));
    assert_eq!(rights.roles.len(), 2);
    drop(read);

    // Cycles, unknown roles, built-in names and privileges on the wrong
    // object are refused.
    let refused =
        db.write(|tx| tx.put_role(&role("crm-reader", vec![crm(Act::Read)], &["crm-lead"])));
    assert!(refused.unwrap_err().to_string().contains("cycle"));
    assert!(
        db.write(|tx| tx.put_role(&role("x", vec![], &["missing"])))
            .is_err()
    );
    assert!(
        db.write(|tx| tx.put_role(&role("editor", vec![], &[])))
            .is_err()
    );
    let wrong = Privilege::grant(
        Act::Backup,
        Object::Namespace {
            namespace: "crm".into(),
        },
    );
    assert!(
        db.write(|tx| tx.put_role(&role("x", vec![wrong.clone()], &[])))
            .is_err()
    );
    assert!(
        db.write(|tx| tx.put_role(&role(
            "x",
            vec![Privilege::grant(
                Act::Drop,
                Object::Label {
                    namespace: "crm".into(),
                    label: "L".into(),
                    edges: false
                }
            )],
            &[]
        )))
        .is_err()
    );

    // `public` reaches every account.
    db.write(|tx| {
        tx.put_role(&role(
            PUBLIC,
            vec![Privilege::grant(Act::Metrics, Object::Server)],
            &[],
        ))
    })
    .unwrap();
    let read = db.begin_read().unwrap();
    assert!(
        read.rights(&read.account("ana").unwrap().unwrap())
            .unwrap()
            .on_server(Act::Metrics)
    );
    drop(read);

    // Deleting a role removes its grants from roles and accounts.
    db.write(|tx| tx.delete_role("crm-reader").map(|_| ()))
        .unwrap();
    let read = db.begin_read().unwrap();
    assert!(read.role("crm-lead").unwrap().unwrap().roles.is_empty());
    let rights = read.rights(&read.account("ana").unwrap().unwrap()).unwrap();
    assert!(!rights.on_namespace(Act::Read, "crm") && rights.on_namespace(Act::Create, "crm"));
}
