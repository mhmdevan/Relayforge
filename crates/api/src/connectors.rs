use axum::http::HeaderMap;
use domain::DomainEvent;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use std::{collections::HashMap, sync::Arc};
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Generic,
    Github,
    Stripe,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Generic => "generic",
            Provider::Github => "github",
            Provider::Stripe => "stripe",
        }
    }

    pub fn from_source(source: &str) -> Option<Self> {
        match source.to_ascii_lowercase().as_str() {
            "generic" => Some(Provider::Generic),
            "github" => Some(Provider::Github),
            "stripe" => Some(Provider::Stripe),
            _ => None,
        }
    }
}

pub trait Connector: Send + Sync {
    fn provider(&self) -> Provider;

    fn verify(&self, headers: &HeaderMap, body: &[u8], secret: &str) -> Result<(), String>;

    fn parse(&self, body: &[u8]) -> Result<Value, String>;

    fn normalize(&self, parsed: &Value, headers: &HeaderMap) -> Result<DomainEvent, String>;
}

#[derive(Clone)]
pub struct ConnectorRegistry {
    connectors: Arc<HashMap<Provider, Arc<dyn Connector>>>,
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        let mut connectors: HashMap<Provider, Arc<dyn Connector>> = HashMap::new();
        connectors.insert(Provider::Generic, Arc::new(GenericConnector));
        connectors.insert(Provider::Github, Arc::new(GithubConnector));
        connectors.insert(Provider::Stripe, Arc::new(StripeConnector));

        Self {
            connectors: Arc::new(connectors),
        }
    }
}

impl ConnectorRegistry {
    pub fn connector_for_source(&self, source: &str) -> Option<Arc<dyn Connector>> {
        let provider = Provider::from_source(source)?;
        self.connectors.get(&provider).cloned()
    }
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn hmac_sha256_hex(secret: &str, payload: &[u8]) -> Result<String, String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| "invalid HMAC secret".to_string())?;
    mac.update(payload);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn constant_time_eq_hex(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GenericPayload {
    event: String,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    fail_until_attempt: Option<i64>,
    #[serde(default)]
    force_permanent_fail: Option<bool>,
}

struct GenericConnector;

impl Connector for GenericConnector {
    fn provider(&self) -> Provider {
        Provider::Generic
    }

    fn verify(&self, headers: &HeaderMap, body: &[u8], secret: &str) -> Result<(), String> {
        let signature =
            header_str(headers, "x-signature").ok_or_else(|| "missing X-Signature".to_string())?;

        let expected = hmac_sha256_hex(secret, body)?;
        if constant_time_eq_hex(&expected, &signature) {
            Ok(())
        } else {
            Err("invalid signature".to_string())
        }
    }

    fn parse(&self, body: &[u8]) -> Result<Value, String> {
        let payload: GenericPayload = serde_json::from_slice(body)
            .map_err(|_| "invalid generic payload schema".to_string())?;

        if payload.event.trim().is_empty() {
            return Err("generic payload field 'event' must be non-empty".to_string());
        }

        if let Some(attempt) = payload.fail_until_attempt {
            if !(0..=10_000).contains(&attempt) {
                return Err(
                    "generic payload field 'fail_until_attempt' is out of allowed range"
                        .to_string(),
                );
            }
        }

        serde_json::to_value(payload).map_err(|_| "failed to parse generic payload".to_string())
    }

    fn normalize(&self, parsed: &Value, headers: &HeaderMap) -> Result<DomainEvent, String> {
        let event_type = parsed
            .get("event")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "generic payload missing field 'event'".to_string())?
            .to_string();

        let provider_event_id = header_str(headers, "x-event-id");
        let data = parsed.get("data").cloned().unwrap_or(Value::Null);

        Ok(DomainEvent {
            source: self.provider().as_str().to_string(),
            event_type,
            provider_event_id,
            occurred_at_unix: None,
            data,
        })
    }
}

struct GithubConnector;

impl Connector for GithubConnector {
    fn provider(&self) -> Provider {
        Provider::Github
    }

    fn verify(&self, headers: &HeaderMap, body: &[u8], secret: &str) -> Result<(), String> {
        let signature = header_str(headers, "x-hub-signature-256")
            .ok_or_else(|| "missing X-Hub-Signature-256".to_string())?;

        let signature = signature
            .strip_prefix("sha256=")
            .ok_or_else(|| "invalid X-Hub-Signature-256 format".to_string())?;

        let expected = hmac_sha256_hex(secret, body)?;
        if constant_time_eq_hex(&expected, signature) {
            Ok(())
        } else {
            Err("invalid signature".to_string())
        }
    }

    fn parse(&self, body: &[u8]) -> Result<Value, String> {
        let parsed: Value =
            serde_json::from_slice(body).map_err(|_| "invalid JSON payload".to_string())?;
        if !parsed.is_object() {
            return Err("github payload must be a JSON object".to_string());
        }

        Ok(parsed)
    }

    fn normalize(&self, parsed: &Value, headers: &HeaderMap) -> Result<DomainEvent, String> {
        let event_type = header_str(headers, "x-github-event")
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                parsed
                    .get("action")
                    .and_then(Value::as_str)
                    .map(|v| format!("github.{v}"))
            })
            .unwrap_or_else(|| "github.unknown".to_string());

        let provider_event_id = header_str(headers, "x-github-delivery");

        Ok(DomainEvent {
            source: self.provider().as_str().to_string(),
            event_type,
            provider_event_id,
            occurred_at_unix: None,
            data: parsed.clone(),
        })
    }
}

struct StripeConnector;

impl Connector for StripeConnector {
    fn provider(&self) -> Provider {
        Provider::Stripe
    }

    fn verify(&self, headers: &HeaderMap, body: &[u8], secret: &str) -> Result<(), String> {
        let signature = header_str(headers, "stripe-signature")
            .ok_or_else(|| "missing Stripe-Signature".to_string())?;

        let (timestamp, signatures) = parse_stripe_signature(&signature)?;
        let body_utf8 = std::str::from_utf8(body)
            .map_err(|_| "stripe payload must be valid UTF-8 JSON".to_string())?;
        let signed_payload = format!("{timestamp}.{body_utf8}");
        let expected = hmac_sha256_hex(secret, signed_payload.as_bytes())?;

        if signatures
            .iter()
            .any(|candidate| constant_time_eq_hex(&expected, candidate))
        {
            Ok(())
        } else {
            Err("invalid signature".to_string())
        }
    }

    fn parse(&self, body: &[u8]) -> Result<Value, String> {
        let parsed: Value =
            serde_json::from_slice(body).map_err(|_| "invalid stripe payload".to_string())?;
        if !parsed.is_object() {
            return Err("stripe payload must be a JSON object".to_string());
        }

        let event_type = parsed.get("type").and_then(Value::as_str).map(str::trim);
        if event_type.filter(|v| !v.is_empty()).is_none() {
            return Err("stripe payload missing field 'type'".to_string());
        }

        Ok(parsed)
    }

    fn normalize(&self, parsed: &Value, _headers: &HeaderMap) -> Result<DomainEvent, String> {
        let event_type = parsed
            .get("type")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| "stripe payload missing field 'type'".to_string())?
            .to_string();

        let provider_event_id = parsed
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let occurred_at_unix = parsed.get("created").and_then(Value::as_i64);
        let data = parsed
            .get("data")
            .and_then(|v| v.get("object"))
            .cloned()
            .unwrap_or_else(|| parsed.clone());

        Ok(DomainEvent {
            source: self.provider().as_str().to_string(),
            event_type,
            provider_event_id,
            occurred_at_unix,
            data,
        })
    }
}

fn parse_stripe_signature(raw: &str) -> Result<(String, Vec<String>), String> {
    let mut timestamp: Option<String> = None;
    let mut signatures = Vec::new();

    for part in raw.split(',') {
        let piece = part.trim();
        if let Some(value) = piece.strip_prefix("t=") {
            if !value.trim().is_empty() {
                timestamp = Some(value.trim().to_string());
            }
        }
        if let Some(value) = piece.strip_prefix("v1=") {
            let value = value.trim();
            if !value.is_empty() {
                signatures.push(value.to_string());
            }
        }
    }

    let timestamp = timestamp.ok_or_else(|| "Stripe-Signature missing timestamp".to_string())?;
    if signatures.is_empty() {
        return Err("Stripe-Signature missing v1 signature".to_string());
    }

    Ok((timestamp, signatures))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripe_signature_parser_handles_multiple_v1_values() {
        let raw = "t=1710000000, v1=abc, v1=def";
        let (ts, v1s) = parse_stripe_signature(raw).expect("expected parser success");
        assert_eq!(ts, "1710000000");
        assert_eq!(v1s, vec!["abc".to_string(), "def".to_string()]);
    }
}
