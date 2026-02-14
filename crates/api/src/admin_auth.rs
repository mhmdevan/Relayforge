use crate::AppState;
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AdminRole {
    Viewer,
    Operator,
    Admin,
}

impl AdminRole {
    pub fn from_str(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "viewer" => Some(Self::Viewer),
            "operator" => Some(Self::Operator),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Operator => "operator",
            Self::Admin => "admin",
        }
    }

    pub fn allows(self, required: Self) -> bool {
        self.rank() >= required.rank()
    }

    fn rank(self) -> u8 {
        match self {
            Self::Viewer => 1,
            Self::Operator => 2,
            Self::Admin => 3,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdminPrincipal {
    pub key_id: Option<Uuid>,
    pub role: AdminRole,
}

#[derive(Debug, Deserialize)]
struct FallbackApiKeyRaw {
    key: String,
    role: String,
}

#[derive(Clone, Default)]
pub struct AdminAccessControl {
    fallback_by_hash: Arc<HashMap<String, AdminRole>>,
}

impl AdminAccessControl {
    pub fn from_json(raw_json: Option<&str>) -> anyhow::Result<Self> {
        let raw = raw_json.unwrap_or("").trim();
        if raw.is_empty() {
            return Ok(Self::default());
        }

        let entries: Vec<FallbackApiKeyRaw> = serde_json::from_str(raw).map_err(|e| {
            anyhow::anyhow!("ADMIN__FALLBACK_API_KEYS_JSON must be valid JSON array: {e}")
        })?;

        let mut fallback_by_hash = HashMap::new();
        for entry in entries {
            if entry.key.trim().is_empty() {
                anyhow::bail!("admin fallback key must not be empty");
            }

            let role = AdminRole::from_str(&entry.role)
                .ok_or_else(|| anyhow::anyhow!("invalid admin role: {}", entry.role))?;

            fallback_by_hash.insert(sha256_hex(entry.key.as_bytes()), role);
        }

        Ok(Self {
            fallback_by_hash: Arc::new(fallback_by_hash),
        })
    }

    pub fn resolve_fallback_role_for_token(&self, token: &str) -> Option<AdminRole> {
        let hash = sha256_hex(token.as_bytes());
        self.fallback_by_hash.get(&hash).copied()
    }
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

fn json_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ErrorBody { error: msg.into() })).into_response()
}

fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
}

fn extract_bearer_token(headers: &HeaderMap) -> Result<&str, &'static str> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or("missing Authorization header")?;

    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .ok_or("Authorization must use Bearer scheme")?
        .trim();

    if token.is_empty() {
        return Err("Bearer token must not be empty");
    }

    Ok(token)
}

pub async fn require_operator_or_admin(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let token = match extract_bearer_token(req.headers()) {
        Ok(v) => v,
        Err(msg) => return json_error(StatusCode::UNAUTHORIZED, msg),
    };

    let identity = if let Some(role) = state
        .admin_access_control
        .resolve_fallback_role_for_token(token)
    {
        AdminPrincipal { role, key_id: None }
    } else {
        let token_hash = sha256_hex(token.as_bytes());
        let db_key =
            match infra::admin_api_keys::find_active_by_hash(&state.pool, &token_hash).await {
                Ok(Some(v)) => v,
                Ok(None) => return json_error(StatusCode::UNAUTHORIZED, "invalid admin API key"),
                Err(_) => {
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "failed to validate admin API key",
                    )
                }
            };

        let role = match AdminRole::from_str(&db_key.role) {
            Some(v) => v,
            None => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("invalid role in admin_api_keys: {}", db_key.role),
                )
            }
        };

        AdminPrincipal {
            role,
            key_id: Some(db_key.id),
        }
    };

    if !identity.role.allows(AdminRole::Operator) {
        return json_error(
            StatusCode::FORBIDDEN,
            format!(
                "insufficient role: required=operator actual={}",
                identity.role.as_str()
            ),
        );
    }

    req.extensions_mut().insert(identity);
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_order_is_enforced() {
        assert!(AdminRole::Admin.allows(AdminRole::Operator));
        assert!(AdminRole::Operator.allows(AdminRole::Operator));
        assert!(!AdminRole::Viewer.allows(AdminRole::Operator));
    }
}
