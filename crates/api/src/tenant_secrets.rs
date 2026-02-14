use crate::connectors::Provider;
use anyhow::Context;
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TenantProviderSecret {
    pub tenant_id: Uuid,
    pub provider: Provider,
    pub secret: String,
}

#[derive(Debug, Deserialize)]
struct TenantProviderSecretRaw {
    tenant_id: String,
    provider: String,
    secret: String,
}

#[derive(Clone, Default)]
pub struct TenantSecretStore {
    fallback: Arc<HashMap<Provider, String>>,
    per_tenant: Arc<HashMap<Uuid, HashMap<Provider, String>>>,
}

impl TenantSecretStore {
    pub fn from_entries(
        fallback: HashMap<Provider, String>,
        entries: Vec<TenantProviderSecret>,
    ) -> anyhow::Result<Self> {
        let mut per_tenant: HashMap<Uuid, HashMap<Provider, String>> = HashMap::new();

        for entry in entries {
            if entry.secret.trim().is_empty() {
                anyhow::bail!(
                    "tenant/provider secret must not be empty for tenant_id={} provider={}",
                    entry.tenant_id,
                    entry.provider.as_str()
                );
            }

            per_tenant
                .entry(entry.tenant_id)
                .or_default()
                .insert(entry.provider, entry.secret);
        }

        Ok(Self {
            fallback: Arc::new(fallback),
            per_tenant: Arc::new(per_tenant),
        })
    }

    pub fn from_json(
        fallback: HashMap<Provider, String>,
        raw_json: Option<&str>,
    ) -> anyhow::Result<Self> {
        let raw = raw_json.unwrap_or("").trim();
        if raw.is_empty() {
            return Self::from_entries(fallback, Vec::new());
        }

        let raw_entries: Vec<TenantProviderSecretRaw> = serde_json::from_str(raw)
            .context("TENANT__PROVIDER_SECRETS_JSON must be a JSON array")?;

        let mut entries = Vec::with_capacity(raw_entries.len());
        for raw_entry in raw_entries {
            let tenant_id: Uuid = raw_entry
                .tenant_id
                .parse()
                .with_context(|| format!("invalid tenant_id: {}", raw_entry.tenant_id))?;

            let provider = Provider::from_source(&raw_entry.provider).with_context(|| {
                format!(
                    "invalid provider: {} (expected generic/github/stripe)",
                    raw_entry.provider
                )
            })?;

            entries.push(TenantProviderSecret {
                tenant_id,
                provider,
                secret: raw_entry.secret,
            });
        }

        Self::from_entries(fallback, entries)
    }

    pub fn resolve_secret(&self, tenant_id: Uuid, provider: Provider) -> Option<String> {
        self.per_tenant
            .get(&tenant_id)
            .and_then(|providers| providers.get(&provider))
            .cloned()
            .or_else(|| self.fallback.get(&provider).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_secret_overrides_fallback() {
        let tenant_id = Uuid::new_v4();

        let mut fallback = HashMap::new();
        fallback.insert(Provider::Generic, "fallback-generic".to_string());

        let store = TenantSecretStore::from_entries(
            fallback,
            vec![TenantProviderSecret {
                tenant_id,
                provider: Provider::Generic,
                secret: "tenant-specific".to_string(),
            }],
        )
        .expect("expected tenant secret store to build");

        assert_eq!(
            store.resolve_secret(tenant_id, Provider::Generic),
            Some("tenant-specific".to_string())
        );
    }
}
