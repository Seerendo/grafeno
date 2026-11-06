//! The cost of roles (phase 14.2): `roles` roles, each with privileges on
//! its own namespace, a denial, a label privilege and two roles below it,
//! and an account holding `held` of them. Measures reading and computing
//! the account's rights once (what a change of roles costs the next
//! request), and checking a namespace, a label and a property afterwards
//! (what every request costs).
//!
//! `cargo run --release -p grafeno --example rights_bench [roles] [held]`

use std::time::Instant;

use grafeno::{Account, AccountKind, Act, Grafeno, Object, Privilege, RoleDef, Timestamp};

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let roles = args.first().copied().unwrap_or(5_000);
    let held = args.get(1).copied().unwrap_or(1_000).min(roles);
    let db = Grafeno::in_memory().unwrap();
    let started = Instant::now();
    db.write(|tx| {
        for i in 0..roles {
            let ns = format!("ns{i}");
            let below: Vec<String> = [i / 2, i / 3]
                .into_iter()
                .filter(|j| *j < i)
                .map(|j| format!("role{j}"))
                .collect();
            tx.put_role(&RoleDef {
                name: format!("role{i}"),
                privileges: vec![
                    Privilege::grant(
                        Act::Read,
                        Object::Namespace {
                            namespace: ns.clone(),
                        },
                    ),
                    Privilege::grant(
                        Act::Create,
                        Object::Namespace {
                            namespace: ns.clone(),
                        },
                    ),
                    Privilege::deny(
                        Act::Delete,
                        Object::Namespace {
                            namespace: format!("team{}-*", i % 50),
                        },
                    ),
                    Privilege::deny(
                        Act::Read,
                        Object::Label {
                            namespace: ns,
                            label: "Salary".into(),
                            edges: false,
                        },
                    ),
                ],
                grants: Vec::new(),
                roles: below,
                description: None,
                created_at: Timestamp::now(),
            })?;
        }
        tx.create_account(&Account {
            name: "acct".into(),
            kind: AccountKind::User,
            password_hash: None,
            grants: Vec::new(),
            roles: (0..held)
                .map(|i| format!("role{}", roles - 1 - i))
                .collect(),
            superuser: false,
            own_only: false,
            disabled: false,
            expires_at: None,
            description: None,
            created_at: Timestamp::now(),
        })
    })
    .unwrap();
    let setup_s = started.elapsed().as_secs_f64();
    let read = db.begin_read().unwrap();
    let account = read.account("acct").unwrap().unwrap();
    let started = Instant::now();
    let rights = read.rights(&account).unwrap();
    let compute_ms = started.elapsed().as_secs_f64() * 1000.0;
    let checks = 1_000_000;
    let started = Instant::now();
    let mut allowed = 0usize;
    for i in 0..checks {
        let ns = format!("ns{}", i % roles);
        allowed += usize::from(rights.on_namespace(Act::Read, &ns));
    }
    let namespace_ns = started.elapsed().as_secs_f64() * 1e9 / checks as f64;
    // The cost of building the name, measured alone, comes off.
    let started = Instant::now();
    let mut names = 0usize;
    for i in 0..checks {
        names += format!("ns{}", i % roles).len();
    }
    let naming_ns = started.elapsed().as_secs_f64() * 1e9 / checks as f64;
    let started = Instant::now();
    for i in 0..checks {
        let ns = format!("ns{}", i % roles);
        allowed += usize::from(rights.on_label(Act::Read, &ns, "Salary", false));
        allowed += usize::from(rights.on_property(Act::Read, &ns, "Person", "email"));
    }
    let label_ns = started.elapsed().as_secs_f64() * 1e9 / (2 * checks) as f64;
    println!("measure,value,unit");
    println!("roles,{roles},");
    println!("roles held directly,{held},");
    println!("roles reached,{},", rights.roles.len());
    println!("writing the roles,{setup_s:.1},s");
    println!("reading and computing the rights,{compute_ms:.1},ms");
    println!(
        "namespace check,{:.0},ns",
        (namespace_ns - naming_ns).max(0.0)
    );
    println!(
        "label or property check,{:.0},ns",
        (label_ns - naming_ns).max(0.0)
    );
    eprintln!("({allowed} allowed, {names} name bytes)");
}
