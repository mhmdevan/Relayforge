use api::{
    connectors::{ConnectorRegistry, Provider},
    tenant_secrets::{TenantProviderSecret, TenantSecretStore},
};
use axum::http::{HeaderMap, HeaderValue};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

#[test]
fn tenant_specific_secret_verification_works() -> anyhow::Result<()> {
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    let store = TenantSecretStore::from_entries(
        HashMap::new(),
        vec![
            TenantProviderSecret {
                tenant_id: tenant_a,
                provider: Provider::Generic,
                secret: "tenant-a-generic-secret".to_string(),
            },
            TenantProviderSecret {
                tenant_id: tenant_b,
                provider: Provider::Generic,
                secret: "tenant-b-generic-secret".to_string(),
            },
        ],
    )?;

    let registry = ConnectorRegistry::default();
    let connector = registry
        .connector_for_source("generic")
        .expect("generic connector must exist");

    let payload = br#"{"event":"invoice.paid","data":{"invoice_id":"inv_1"}}"#;

    let tenant_a_secret = store
        .resolve_secret(tenant_a, Provider::Generic)
        .expect("tenant a secret missing");
    let tenant_b_secret = store
        .resolve_secret(tenant_b, Provider::Generic)
        .expect("tenant b secret missing");

    let sig_a = sign_hex(&tenant_a_secret, payload)?;
    let sig_b = sign_hex(&tenant_b_secret, payload)?;

    let mut headers_a = HeaderMap::new();
    headers_a.insert("x-signature", HeaderValue::from_str(&sig_a)?);

    let mut headers_b = HeaderMap::new();
    headers_b.insert("x-signature", HeaderValue::from_str(&sig_b)?);

    assert!(connector
        .verify(&headers_a, payload, &tenant_a_secret)
        .is_ok());
    assert!(connector
        .verify(&headers_b, payload, &tenant_b_secret)
        .is_ok());

    // Cross-tenant signatures must fail.
    assert!(connector
        .verify(&headers_b, payload, &tenant_a_secret)
        .is_err());
    assert!(connector
        .verify(&headers_a, payload, &tenant_b_secret)
        .is_err());

    Ok(())
}

#[test]
fn connectors_normalize_to_standard_domain_event() -> anyhow::Result<()> {
    let registry = ConnectorRegistry::default();

    let generic = registry
        .connector_for_source("generic")
        .expect("generic connector must exist");
    let github = registry
        .connector_for_source("github")
        .expect("github connector must exist");
    let stripe = registry
        .connector_for_source("stripe")
        .expect("stripe connector must exist");

    let generic_body = br#"{"event":"order.created","data":{"order_id":"ord_1"}}"#;
    let generic_secret = "generic-tenant-secret";
    let generic_sig = sign_hex(generic_secret, generic_body)?;
    let mut generic_headers = HeaderMap::new();
    generic_headers.insert("x-signature", HeaderValue::from_str(&generic_sig)?);
    generic_headers.insert("x-event-id", HeaderValue::from_static("evt-generic-1"));

    let generic_event = run_pipeline(
        generic.as_ref(),
        &generic_headers,
        generic_body,
        generic_secret,
    )?;
    assert_eq!(generic_event.source, "generic");
    assert_eq!(generic_event.event_type, "order.created");
    assert_eq!(
        generic_event.provider_event_id.as_deref(),
        Some("evt-generic-1")
    );

    let github_body = br#"{"action":"opened","repository":{"full_name":"org/repo"}}"#;
    let github_secret = "github-tenant-secret";
    let github_sig = sign_hex(github_secret, github_body)?;
    let mut github_headers = HeaderMap::new();
    github_headers.insert(
        "x-hub-signature-256",
        HeaderValue::from_str(&format!("sha256={github_sig}"))?,
    );
    github_headers.insert(
        "x-github-delivery",
        HeaderValue::from_static("gh-delivery-1"),
    );
    github_headers.insert("x-github-event", HeaderValue::from_static("issues"));

    let github_event = run_pipeline(github.as_ref(), &github_headers, github_body, github_secret)?;
    assert_eq!(github_event.source, "github");
    assert_eq!(github_event.event_type, "issues");
    assert_eq!(
        github_event.provider_event_id.as_deref(),
        Some("gh-delivery-1")
    );

    let stripe_body = br#"{"id":"evt_123","type":"payment_intent.succeeded","created":1710000000,"data":{"object":{"amount":1000}}}"#;
    let stripe_secret = "stripe-tenant-secret";
    let stripe_ts = "1710000000";
    let stripe_sig = sign_hex(
        stripe_secret,
        format!("{stripe_ts}.{}", std::str::from_utf8(stripe_body)?).as_bytes(),
    )?;
    let mut stripe_headers = HeaderMap::new();
    stripe_headers.insert(
        "stripe-signature",
        HeaderValue::from_str(&format!("t={stripe_ts},v1={stripe_sig}"))?,
    );

    let stripe_event = run_pipeline(stripe.as_ref(), &stripe_headers, stripe_body, stripe_secret)?;
    assert_eq!(stripe_event.source, "stripe");
    assert_eq!(stripe_event.event_type, "payment_intent.succeeded");
    assert_eq!(stripe_event.provider_event_id.as_deref(), Some("evt_123"));
    assert_eq!(stripe_event.occurred_at_unix, Some(1710000000));

    Ok(())
}

fn run_pipeline(
    connector: &dyn api::connectors::Connector,
    headers: &HeaderMap,
    body: &[u8],
    secret: &str,
) -> anyhow::Result<domain::DomainEvent> {
    connector
        .verify(headers, body, secret)
        .map_err(anyhow::Error::msg)?;
    let parsed = connector.parse(body).map_err(anyhow::Error::msg)?;
    connector
        .normalize(&parsed, headers)
        .map_err(anyhow::Error::msg)
}

fn sign_hex(secret: &str, payload: &[u8]) -> anyhow::Result<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())?;
    mac.update(payload);
    Ok(hex::encode(mac.finalize().into_bytes()))
}
