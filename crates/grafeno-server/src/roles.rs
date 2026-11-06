//! Roles (phase 14.2): `/v1/roles` to define them (callers with the
//! `manage_roles` privilege), and `/v1/privileges`, the catalog of what a
//! role can grant.
//!
//! A role holds privileges (an action on the server, a namespace pattern,
//! a label or a property, granted or denied), built-in levels on
//! namespace patterns, and other roles. An account's rights are the union
//! of its grants and every role it reaches; a denial wins.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use grafeno::{Act, Grant, Privilege, RoleDef, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::{ApiError, ApiResult, AppState, Body};

/// `GET /v1/privileges`: every action, by what it applies to, and the
/// built-in roles.
pub async fn catalog() -> Json<Value> {
    let names = |acts: &[Act]| acts.iter().map(|a| a.name()).collect::<Vec<_>>();
    let levels: Vec<Value> = [
        grafeno::Role::Read,
        grafeno::Role::Write,
        grafeno::Role::Admin,
    ]
    .into_iter()
    .map(|role| {
        json!({
            "name": grafeno::level_name(role),
            "on": "namespace",
            "actions": names(grafeno::level_actions(role)),
        })
    })
    .collect();
    Json(json!({
        "server": names(&Act::SERVER),
        "namespace": names(&Act::NAMESPACE),
        "label": names(&[Act::Read, Act::Write]),
        "property": names(&[Act::Read, Act::Write]),
        "built_in": {
            "levels": levels,
            "superuser": "every action on everything",
            "public": format!("a role named {:?}, when defined, is every account's", grafeno::PUBLIC),
        },
    }))
}

/// `GET /v1/roles`.
pub async fn list_roles(State(state): State<AppState>) -> ApiResult<Json<Vec<RoleDef>>> {
    Ok(Json(state.db.begin_read()?.roles()?))
}

/// `GET /v1/roles/{name}`.
pub async fn get_role(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<RoleDef>> {
    state
        .db
        .begin_read()?
        .role(&name)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found_message(format!("no role {name:?}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleInput {
    #[serde(default)]
    privileges: Vec<Privilege>,
    #[serde(default)]
    grants: Vec<Grant>,
    #[serde(default)]
    roles: Vec<String>,
    /// Record policies (14.4).
    #[serde(default)]
    policies: Vec<grafeno::Policy>,
    description: Option<String>,
}

/// `PUT /v1/roles/{name}`: defines or replaces a role (201 when new). A
/// role that would reach itself is refused.
pub async fn put_role(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Body(input): Body<RoleInput>,
) -> ApiResult<(StatusCode, Json<RoleDef>)> {
    let input = Arc::new(input);
    let (role, created) = crate::http::write(&state, move |tx| {
        let old = tx.role(&name)?;
        let role = RoleDef {
            name: name.clone(),
            privileges: input.privileges.clone(),
            grants: input.grants.clone(),
            roles: input.roles.clone(),
            policies: input.policies.clone(),
            description: input.description.clone().filter(|d| !d.trim().is_empty()),
            created_at: old.as_ref().map_or_else(Timestamp::now, |o| o.created_at),
        };
        tx.put_role(&role)?;
        Ok((role, old.is_none()))
    })
    .await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(role)))
}

/// `DELETE /v1/roles/{name}`: removes a role, and every account and role
/// that had it loses it.
pub async fn delete_role(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let target = name.clone();
    if crate::http::write(&state, move |tx| tx.delete_role(&target)).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found_message(format!("no role {name:?}")))
    }
}
