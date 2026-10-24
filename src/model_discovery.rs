//! Account-scoped live model discovery. Cached lists never establish model availability.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::auth::{AuthResolver, ProviderKind, StoredCredential};
use crate::config::Config;
use crate::provider::{ModelEntry, is_agent_model};

const TTL: u64 = 6 * 60 * 60;
const RETRY_DELAY: u64 = 60;
const QUERY_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    version: u32,
    fetched_at: u64,
    models: Vec<ModelEntry>,
}

#[derive(Default)]
struct Slot {
    record: Option<Record>,
    retry_after: u64,
    refreshing: bool,
}

struct Cache {
    directory: PathBuf,
    slots: Mutex<HashMap<String, Slot>>,
    gates: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    updates: watch::Sender<u64>,
}

static CACHE: LazyLock<Cache> = LazyLock::new(|| {
    Cache::new(
        dirs::cache_dir()
            .unwrap_or_else(|| crate::session::storage::config_path().join("cache"))
            .join("zerostack/model-discovery"),
    )
});

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// No raw credential, URL, or workspace identifier is written to cache filenames.
fn scope_key(
    provider: &str,
    endpoint: Option<&str>,
    identity: &str,
    headers: &BTreeMap<String, String>,
) -> String {
    let bytes = serde_json::to_vec(&(1, provider, endpoint, identity, headers))
        .expect("serializable discovery scope");
    format!("{:x}", Sha256::digest(bytes))
}

fn codex_identity(account: &str, access: &str) -> anyhow::Result<String> {
    let claims = access
        .split('.')
        .nth(1)
        .and_then(|part| base64::prelude::BASE64_URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    // A team workspace can contain multiple users with different entitlements.
    // Decode only to partition the cache, never to authenticate a token. Opaque
    // tokens get a token-specific cache rather than sharing an unknown identity.
    let user = claims
        .as_ref()
        .and_then(|claims| {
            claims
                .get("sub")
                .or_else(|| claims.pointer("/https:~1~1api.openai.com~1auth/chatgpt_user_id"))
        })
        .and_then(serde_json::Value::as_str)
        .unwrap_or(access);
    Ok(serde_json::to_string(&("codex-oauth", account, user))?)
}
#[derive(Clone)]
struct Query {
    provider: String,
    cli_key: Option<String>,
    cfg: Config,
    key: String,
}

impl Query {
    fn new(provider: &str, cli_key: Option<&str>, cfg: &Config) -> anyhow::Result<Self> {
        let provider = if cfg.custom_providers_map().contains_key(provider) {
            provider.to_string()
        } else {
            crate::config::commands::canonical_provider_name(provider)
        };
        let custom = cfg.custom_providers_map();
        let resolved = crate::provider::resolve_provider_config(&provider, &custom)?;
        let auth: HashMap<String, StoredCredential> = match std::fs::read(crate::auth::auth_path())
        {
            Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => HashMap::new(),
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e.into()),
        };
        let identity = match (
            resolved.kind,
            cli_key,
            auth.get("opencode-go"),
            auth.get("openai-codex"),
        ) {
            (ProviderKind::OpenCodeGo, None, Some(StoredCredential::OpenCodeOAuth(cred)), _) => {
                serde_json::to_string(&("opencode-oauth", &cred.account_id, &cred.org_id))?
            }
            (ProviderKind::OpenAICodex, _, _, Some(StoredCredential::OAuth(cred))) => {
                codex_identity(&cred.account_id, &cred.access)?
            }
            (ProviderKind::OpenAICodex, _, _, _) => {
                anyhow::bail!("run 'zerostack auth login codex' before listing Codex models")
            }
            _ => {
                let key = AuthResolver::new(resolved.kind)
                    .with_cli_key(cli_key)
                    .with_env_override(resolved.api_key_env.as_deref())
                    .with_auth_keys(Some(&crate::auth::stored_api_keys()?))
                    .with_config_keys(cfg.api_keys.as_ref())
                    .with_custom_provider_name(Some(&provider))
                    .resolve();
                let key = match key {
                    Ok(key) => key,
                    Err(_) if custom.contains_key(&provider) => String::new(),
                    Err(error) => return Err(error),
                };
                serde_json::to_string(&("api-key", key))?
            }
        };
        let headers: BTreeMap<_, _> = custom
            .get(&provider)
            .map(|custom| {
                custom
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        Ok((
                            name.to_ascii_lowercase(),
                            crate::provider::expand_env(value)?,
                        ))
                    })
                    .collect::<anyhow::Result<_>>()
            })
            .transpose()?
            .unwrap_or_default();
        let scope = format!(
            "{provider}:{:?}:{}",
            resolved.kind, resolved.danger_accept_invalid_certs
        );
        let key = scope_key(&scope, resolved.base_url.as_deref(), &identity, &headers);
        Ok(Self {
            provider,
            cli_key: cli_key.map(str::to_string),
            cfg: cfg.clone(),
            key,
        })
    }

    async fn fetch(&self) -> anyhow::Result<Vec<ModelEntry>> {
        let future = async {
            let custom = self.cfg.custom_providers_map();
            let models = if custom.contains_key(&self.provider) {
                crate::provider::list_models_manual(
                    &self.provider,
                    self.cli_key.as_deref(),
                    &custom,
                    self.cfg.api_keys.as_ref(),
                )
                .await?
            } else {
                // Build the listing client from this scope, not from an older session's credentials.
                crate::provider::create_client(
                    &self.provider,
                    self.cli_key.as_deref(),
                    &custom,
                    self.cfg.api_keys.as_ref(),
                    None,
                )?
                .list_models()
                .await?
            };
            // Do not publish a result if login/workspace/key changed during discovery.
            anyhow::ensure!(
                Self::new(&self.provider, self.cli_key.as_deref(), &self.cfg)?.key == self.key,
                "model discovery credentials changed; retry the model query"
            );
            prepare_models(&self.provider, models)
        };
        tokio::time::timeout(QUERY_TIMEOUT, future)
            .await
            .map_err(|_| anyhow::anyhow!("model discovery timed out after 15 seconds"))?
    }
}

fn prepare_models(provider: &str, mut models: Vec<ModelEntry>) -> anyhow::Result<Vec<ModelEntry>> {
    anyhow::ensure!(
        models
            .iter()
            .all(|model| !model.id.trim().is_empty() && !model.id.chars().any(char::is_control)),
        "provider returned an invalid model ID"
    );
    models.retain(is_agent_model);
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    for model in &mut models {
        // Metadata may fill a missing limit, but never adds an available model.
        if model.context_length.is_none() {
            model.context_length = crate::models_catalog::catalog_entries(provider)
                .and_then(|entries| entries.iter().find(|entry| entry.id == model.id))
                .and_then(|entry| entry.context_length);
        }
    }
    Ok(models)
}
impl Cache {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            slots: Mutex::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
            updates: watch::channel(0).0,
        }
    }

    fn snapshot(&self, key: &str) -> Option<Record> {
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.entry(key.to_string()).or_insert_with(|| Slot {
            record: read_record(&self.directory.join(format!("{key}.json"))),
            ..Slot::default()
        });
        slot.record.clone()
    }

    fn reserve(&self, key: &str) -> bool {
        self.snapshot(key);
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.get_mut(key).unwrap();
        if slot.refreshing
            || slot.retry_after > now()
            || slot
                .record
                .as_ref()
                .is_some_and(|record| now().saturating_sub(record.fetched_at) < TTL)
        {
            return false;
        }
        slot.refreshing = true;
        true
    }

    async fn refresh<F, Fut>(
        &self,
        key: &str,
        force: bool,
        fetch: F,
    ) -> anyhow::Result<Arc<[ModelEntry]>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Vec<ModelEntry>>>,
    {
        let gate = self
            .gates
            .lock()
            .unwrap()
            .entry(key.to_string())
            .or_default()
            .clone();
        let _guard = gate.lock().await;
        if !force
            && let Some(record) = self.snapshot(key)
            && now().saturating_sub(record.fetched_at) < TTL
        {
            self.slots.lock().unwrap().get_mut(key).unwrap().refreshing = false;
            return Ok(Arc::from(record.models));
        }
        let result = fetch().await;
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.entry(key.to_string()).or_default();
        slot.refreshing = false;
        match result {
            Ok(models) => {
                let record = Record {
                    version: 1,
                    fetched_at: now(),
                    models,
                };
                if let Err(error) = write_record(&self.directory, key, &record) {
                    tracing::warn!("could not persist model discovery cache: {error}");
                }
                let models = Arc::from(record.models.clone());
                slot.record = Some(record);
                slot.retry_after = 0;
                drop(slots);
                self.updates
                    .send_modify(|revision| *revision = revision.wrapping_add(1));
                Ok(models)
            }
            Err(error) => {
                slot.retry_after = now().saturating_add(RETRY_DELAY);
                Err(error)
            }
        }
    }
}

fn read_record(path: &Path) -> Option<Record> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_CACHE_BYTES {
        return None;
    }
    let record: Record = serde_json::from_reader(file).ok()?;
    (record.version == 1
        && record.fetched_at <= now().saturating_add(60)
        && record
            .models
            .iter()
            .all(|model| !model.id.trim().is_empty()))
    .then_some(record)
}

fn write_record(directory: &Path, key: &str, record: &Record) -> anyhow::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let temporary = directory.join(format!(".{key}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer(&mut file, record)?;
        file.flush()?;
        std::fs::rename(&temporary, directory.join(format!("{key}.json")))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

pub fn subscribe() -> watch::Receiver<u64> {
    CACHE.updates.subscribe()
}

/// Immediate snapshot for TUI startup. Cold caches are empty; manual entry remains valid.
pub fn warm(
    provider: &str,
    cli_key: Option<&str>,
    cfg: &Config,
) -> anyhow::Result<Arc<[ModelEntry]>> {
    let query = Query::new(provider, cli_key, cfg)?;
    let snapshot = CACHE
        .snapshot(&query.key)
        .map(|record| Arc::from(record.models))
        .unwrap_or_else(|| Arc::from([]));
    if CACHE.reserve(&query.key) {
        tokio::spawn(async move {
            if let Err(error) = CACHE.refresh(&query.key, false, || query.fetch()).await {
                tracing::debug!("background model discovery failed: {error}");
            }
        });
    }
    Ok(snapshot)
}

pub fn cached_ids(provider: &str, cli_key: Option<&str>, cfg: &Config) -> Vec<String> {
    Query::new(provider, cli_key, cfg)
        .ok()
        .and_then(|query| CACHE.snapshot(&query.key))
        .map(|record| record.models.into_iter().map(|model| model.id).collect())
        .unwrap_or_default()
}

/// CLI and explicit refresh: await discovery only when stale or explicitly requested.
/// A failed automatic refresh preserves the last successful list, including an empty one.
pub async fn list(
    provider: &str,
    cli_key: Option<&str>,
    cfg: &Config,
    force: bool,
) -> anyhow::Result<Arc<[ModelEntry]>> {
    let query = Query::new(provider, cli_key, cfg)?;
    let previous = CACHE.snapshot(&query.key);
    if !force
        && let Some(record) = &previous
        && now().saturating_sub(record.fetched_at) < TTL
    {
        return Ok(Arc::from(record.models.clone()));
    }
    match CACHE.refresh(&query.key, force, || query.fetch()).await {
        Ok(models) => Ok(models),
        Err(error) if !force && previous.is_some() => {
            tracing::warn!("model discovery failed; using the last successful list: {error}");
            Ok(Arc::from(previous.unwrap().models))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
