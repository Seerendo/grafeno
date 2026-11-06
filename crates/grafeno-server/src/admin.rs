//! Managing access over HTTP (phase 14.3): `/v1/admin/users` and
//! `/v1/admin/roles` (the accounts and roles of 14.1 and 14.2 under the
//! names the design gives them), and `/v1/admin/grants`, which grants,
//! denies and revokes privileges and roles with the same rules as `GRANT`,
//! `DENY` and `REVOKE` in GQ.

use axum::Json;
use axum::extract::{Query, State};
use grafeno::{Object, Revoke};
use grafeno_query::GrantRequest;
use serde::Deserialize;
use serde_json::Value;

use crate::auth;
use crate::http::{ApiResult, AppState, Body};

/// Privileges (actions such as `read` or `manage_roles`, or the levels
/// `reader`, `editor`, `admin`) on an object, for roles.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivilegesInput {
    privileges: Vec<String>,
    on: Object,
    /// The roles to grant them to (`POST /v1/admin/grants`).
    #[serde(default)]
    to: Vec<String>,
    /// The roles to revoke them from (`POST /v1/admin/grants/revoke`).
    #[serde(default)]
    from: Vec<String>,
    /// Deny instead of grant.
    #[serde(default)]
    deny: bool,
    /// What a revoke takes: `grants`, `denials` or `both` (the default).
    #[serde(default)]
    which: Option<String>,
}

/// Roles for accounts or other roles.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolesInput {
    roles: Vec<String>,
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    from: Vec<String>,
}

#[derive(Deserialize)]
pub struct HolderQuery {
    holder: String,
}

/// Runs a grant request as the caller; writing ones in the next group
/// commit.
async fn run(state: &AppState, request: GrantRequest) -> ApiResult<Json<Value>> {
    let context = auth::access_context(&auth::current());
    if !request.writes() {
        let result = crate::http::blocking(state, move |db| {
            grafeno_query::manage_grants(&db.begin_read()?, None, request, &context)
                .map_err(engine_error)
        })
        .await?;
        return Ok(Json(result.to_json()));
    }
    let db = state.db.clone();
    let result = crate::http::write(state, move |tx| {
        let read = db.begin_read()?;
        grafeno_query::manage_grants(&read, Some(tx), request.clone(), &context)
            .map_err(engine_error)
    })
    .await?;
    Ok(Json(result.to_json()))
}

fn engine_error(error: grafeno_query::QueryError) -> grafeno::Error {
    if error.forbidden {
        grafeno::Error::Forbidden(error.message)
    } else if error.internal {
        grafeno::Error::Storage(error.message)
    } else {
        grafeno::Error::Invalid(error.message)
    }
}

fn need(list: &[String], what: &str) -> ApiResult<()> {
    if list.is_empty() {
        Err(crate::http::ApiError::bad_request(format!(
            "`{what}` names at least one"
        )))
    } else {
        Ok(())
    }
}

/// `GET /v1/admin/grants?holder=name`: an account's or a role's privileges,
/// with the role each comes from.
pub async fn privileges_of(
    State(state): State<AppState>,
    Query(query): Query<HolderQuery>,
) -> ApiResult<Json<Value>> {
    run(
        &state,
        GrantRequest::Privileges {
            holder: query.holder,
        },
    )
    .await
}

/// `POST /v1/admin/grants`: grants (or, with `deny`, denies) privileges on
/// an object to roles.
pub async fn grant(
    State(state): State<AppState>,
    Body(input): Body<PrivilegesInput>,
) -> ApiResult<Json<Value>> {
    need(&input.to, "to")?;
    run(
        &state,
        GrantRequest::Grant {
            deny: input.deny,
            privileges: input.privileges,
            on: input.on,
            roles: input.to,
        },
    )
    .await
}

/// `POST /v1/admin/grants/revoke`: takes privileges on an object from roles.
pub async fn revoke(
    State(state): State<AppState>,
    Body(input): Body<PrivilegesInput>,
) -> ApiResult<Json<Value>> {
    need(&input.from, "from")?;
    let which = match input.which.as_deref() {
        None | Some("both") => Revoke::Both,
        Some("grants") => Revoke::Grants,
        Some("denials") => Revoke::Denials,
        Some(other) => {
            return Err(crate::http::ApiError::bad_request(format!(
                "`which` is grants, denials or both, not {other:?}"
            )));
        }
    };
    run(
        &state,
        GrantRequest::Revoke {
            which,
            privileges: input.privileges,
            on: input.on,
            roles: input.from,
        },
    )
    .await
}

/// `POST /v1/admin/grants/roles`: gives roles to accounts or other roles.
pub async fn grant_roles(
    State(state): State<AppState>,
    Body(input): Body<RolesInput>,
) -> ApiResult<Json<Value>> {
    need(&input.to, "to")?;
    run(
        &state,
        GrantRequest::GrantRole {
            roles: input.roles,
            to: input.to,
        },
    )
    .await
}

/// `POST /v1/admin/grants/roles/revoke`: takes roles from accounts or other
/// roles.
pub async fn revoke_roles(
    State(state): State<AppState>,
    Body(input): Body<RolesInput>,
) -> ApiResult<Json<Value>> {
    need(&input.from, "from")?;
    run(
        &state,
        GrantRequest::RevokeRole {
            roles: input.roles,
            from: input.from,
        },
    )
    .await
}
