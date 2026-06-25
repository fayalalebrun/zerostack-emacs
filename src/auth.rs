use std::collections::{BTreeSet, HashMap};
use std::env::VarError;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const OPENAI_CODEX_PROVIDER: &str = "openai-codex";

const OPENAI_CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_CODEX_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const OPENAI_CODEX_DEVICE_USER_CODE_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/usercode";
const OPENAI_CODEX_DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const OPENAI_CODEX_DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
const OPENAI_CODEX_DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const OPENAI_CODEX_SCOPE: &str = "openid profile email offline_access";
const OPENAI_CODEX_JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const TOKEN_REFRESH_SKEW_MS: u64 = 60_000;

/// Kind of AI provider
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    OpenRouter,
    OpenAI,
    OpenAICodex,
    Anthropic,
    Gemini,
    Ollama,
}

impl ProviderKind {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "openrouter" => Some(Self::OpenRouter),
            "openai" | "custom" => Some(Self::OpenAI), // "custom" is an alias for OpenAI client
            "openai-codex" | "codex" => Some(Self::OpenAICodex),
            "anthropic" => Some(Self::Anthropic),
            "gemini" | "google" => Some(Self::Gemini),
            "ollama" => Some(Self::Ollama),
            _ => None,
        }
    }
}

/// Resolver for API keys with priority: CLI arg > env var > auth file > config file.
#[derive(Debug, Clone)]
pub struct AuthResolver {
    pub provider_kind: ProviderKind,
    pub api_key_env_override: Option<String>,
    pub cli_key: Option<String>,
    pub config_api_keys: Option<HashMap<String, String>>,
    pub auth_api_keys: Option<HashMap<String, String>>,
    /// Custom provider name (e.g., "local-vllm") for fallback key lookup
    pub custom_provider_name: Option<String>,
}

impl AuthResolver {
    pub fn new(kind: ProviderKind) -> Self {
        Self {
            provider_kind: kind,
            api_key_env_override: None,
            cli_key: None,
            config_api_keys: None,
            auth_api_keys: None,
            custom_provider_name: None,
        }
    }

    pub fn with_cli_key(mut self, key: Option<&str>) -> Self {
        self.cli_key = key.filter(|k| !k.is_empty()).map(String::from);
        self
    }

    pub fn with_env_override(mut self, env_var: Option<&str>) -> Self {
        self.api_key_env_override = env_var.filter(|s| !s.is_empty()).map(String::from);
        self
    }

    pub fn with_config_keys(mut self, keys: Option<&HashMap<String, String>>) -> Self {
        self.config_api_keys = keys.cloned();
        self
    }

    pub fn with_auth_keys(mut self, keys: Option<&HashMap<String, String>>) -> Self {
        self.auth_api_keys = keys.cloned();
        self
    }

    pub fn with_custom_provider_name(mut self, name: Option<&str>) -> Self {
        self.custom_provider_name = name.filter(|s| !s.is_empty()).map(String::from);
        self
    }

    pub fn resolve(&self) -> anyhow::Result<String> {
        self.resolve_with_env(|name| std::env::var(name))
    }

    pub fn resolve_with_env<F: Fn(&str) -> Result<String, VarError>>(
        &self,
        get_env: F,
    ) -> anyhow::Result<String> {
        // Priority 1: CLI argument
        if let Some(ref key) = self.cli_key {
            tracing::warn!(
                "API key provided via --api-key is visible in process listings. \
                 Use the {} environment variable instead.",
                self.env_var_name()
            );
            return Ok(key.clone());
        }

        // Priority 2: Environment variable
        let env_var = self
            .api_key_env_override
            .as_deref()
            .unwrap_or_else(|| self.env_var_name());

        if let Ok(key) = get_env(env_var)
            && !key.is_empty()
        {
            return Ok(key);
        }

        // Priority 3: Auth file (try provider slug first, then custom provider name)
        if let Some(key) = self.key_from_map(self.auth_api_keys.as_ref()) {
            return Ok(key);
        }

        // Priority 4: Config file (try provider slug first, then custom provider name)
        if let Some(key) = self.key_from_map(self.config_api_keys.as_ref()) {
            return Ok(key);
        }

        // Ollama doesn't require an API key
        if self.provider_kind == ProviderKind::Ollama {
            return Ok(String::new());
        }

        anyhow::bail!(
            "No API key found. Set the {} environment variable, run 'zerostack auth set-key {} <key>', add it to config.api_keys under '{}' or '{}', or pass --api-key.",
            env_var,
            self.custom_provider_name
                .as_deref()
                .unwrap_or_else(|| self.provider_slug()),
            self.provider_slug(),
            self.custom_provider_name
                .as_deref()
                .unwrap_or("provider_name")
        )
    }

    fn key_from_map(&self, keys: Option<&HashMap<String, String>>) -> Option<String> {
        if let Some(keys) = keys {
            let slug = self.provider_slug();
            if let Some(key) = keys.get(slug).filter(|k| !k.is_empty()) {
                return Some(key.clone());
            }
            // Fallback to custom provider name for custom providers
            if let Some(ref custom_name) = self.custom_provider_name
                && let Some(key) = keys.get(custom_name).filter(|k| !k.is_empty())
            {
                return Some(key.clone());
            }
        }
        None
    }

    fn env_var_name(&self) -> &'static str {
        match self.provider_kind {
            ProviderKind::OpenAI => "OPENAI_API_KEY",
            ProviderKind::OpenAICodex => "OPENAI_CODEX_API_KEY",
            ProviderKind::Anthropic => "ANTHROPIC_API_KEY",
            ProviderKind::Gemini => "GEMINI_API_KEY",
            ProviderKind::Ollama => "OLLAMA_API_KEY",
            ProviderKind::OpenRouter => "OPENROUTER_API_KEY",
        }
    }

    fn provider_slug(&self) -> &'static str {
        match self.provider_kind {
            ProviderKind::OpenRouter => "openrouter",
            ProviderKind::OpenAI => "openai",
            ProviderKind::OpenAICodex => OPENAI_CODEX_PROVIDER,
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Ollama => "ollama",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum StoredCredential {
    #[serde(rename = "api_key")]
    ApiKey { key: String },
    #[serde(rename = "oauth")]
    OAuth(CodexOAuthCredential),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexOAuthCredential {
    pub access: String,
    pub refresh: String,
    pub expires: u64,
    #[serde(rename = "accountId")]
    pub account_id: String,
}

#[derive(Debug, Clone)]
pub struct CodexRequestAuth {
    pub access_token: String,
    pub account_id: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct AuthFile(HashMap<String, StoredCredential>);

pub fn normalize_auth_provider(provider: &str) -> anyhow::Result<&'static str> {
    match provider.to_lowercase().as_str() {
        "codex" | "openai-codex" => Ok(OPENAI_CODEX_PROVIDER),
        other => anyhow::bail!("unsupported auth provider '{other}' (try 'codex')"),
    }
}

fn credential_provider_key(provider: &str) -> anyhow::Result<String> {
    let provider = provider.trim();
    if provider.is_empty() {
        anyhow::bail!("provider cannot be empty");
    }
    let key = match ProviderKind::from_name(provider) {
        Some(ProviderKind::OpenRouter) => "openrouter".to_string(),
        Some(ProviderKind::OpenAI) => "openai".to_string(),
        Some(ProviderKind::OpenAICodex) => OPENAI_CODEX_PROVIDER.to_string(),
        Some(ProviderKind::Anthropic) => "anthropic".to_string(),
        Some(ProviderKind::Gemini) => "gemini".to_string(),
        Some(ProviderKind::Ollama) => "ollama".to_string(),
        None => provider.to_string(),
    };
    Ok(key)
}

pub fn auth_path() -> PathBuf {
    crate::session::storage::config_path().join("auth.json")
}

pub async fn login_provider(provider: &str, device: bool) -> anyhow::Result<()> {
    let provider = normalize_auth_provider(provider)?;
    if provider != OPENAI_CODEX_PROVIDER {
        anyhow::bail!("unsupported auth provider '{provider}'");
    }

    let credential = if device {
        login_openai_codex_device().await?
    } else {
        login_openai_codex_browser().await?
    };
    set_stored_credential(provider, StoredCredential::OAuth(credential))?;
    println!(
        "logged in to {provider}; credentials saved to {}",
        auth_path().display()
    );
    Ok(())
}

pub fn logout_provider(provider: &str) -> anyhow::Result<()> {
    let provider = normalize_auth_provider(provider)?;
    remove_stored_credential(provider)?;
    println!("logged out of {provider}");
    Ok(())
}

pub fn set_api_key(provider: &str, key: &str) -> anyhow::Result<()> {
    let provider = credential_provider_key(provider)?;
    if key.trim().is_empty() {
        anyhow::bail!("API key cannot be empty");
    }
    set_stored_credential(
        &provider,
        StoredCredential::ApiKey {
            key: key.to_string(),
        },
    )?;
    println!(
        "stored API key for {provider}; credentials saved to {}",
        auth_path().display()
    );
    Ok(())
}

pub fn unset_api_key(provider: &str) -> anyhow::Result<()> {
    let provider = credential_provider_key(provider)?;
    let path = auth_path();
    let _lock = AuthFileLock::acquire(&path)?;
    let mut data = load_auth_file(&path)?;
    match data.0.get(&provider) {
        Some(StoredCredential::ApiKey { .. }) => {
            data.0.remove(&provider);
            save_auth_file(&path, &data)?;
            println!("removed API key for {provider}");
            Ok(())
        }
        Some(StoredCredential::OAuth(_)) => {
            anyhow::bail!(
                "{provider} is configured with subscription auth; use 'zerostack auth logout {provider}'"
            )
        }
        None => {
            println!("no API key stored for {provider}");
            Ok(())
        }
    }
}

pub fn print_auth_status(provider: Option<&str>) -> anyhow::Result<()> {
    let data = load_auth_file(&auth_path())?;
    let providers: Vec<String> = if let Some(provider) = provider {
        vec![credential_provider_key(provider)?]
    } else {
        let mut providers = BTreeSet::new();
        providers.insert(OPENAI_CODEX_PROVIDER.to_string());
        providers.extend(data.0.keys().cloned());
        providers.into_iter().collect()
    };

    println!("auth file: {}", auth_path().display());
    for provider in providers {
        match data.0.get(&provider) {
            Some(StoredCredential::OAuth(cred)) => {
                let status = if cred.expires <= now_millis() {
                    "expired"
                } else {
                    "valid"
                };
                println!(
                    "{provider}: subscription auth configured ({status}, account {}, expires {})",
                    cred.account_id,
                    format_millis(cred.expires)
                );
            }
            Some(StoredCredential::ApiKey { .. }) => {
                println!("{provider}: API key configured");
            }
            None => println!("{provider}: not configured"),
        }
    }
    Ok(())
}

pub fn stored_api_keys() -> anyhow::Result<HashMap<String, String>> {
    api_keys_from_auth_file(&auth_path())
}

fn api_keys_from_auth_file(path: &Path) -> anyhow::Result<HashMap<String, String>> {
    let data = load_auth_file(path)?;
    Ok(data
        .0
        .into_iter()
        .filter_map(|(provider, credential)| match credential {
            StoredCredential::ApiKey { key } if !key.is_empty() => Some((provider, key)),
            _ => None,
        })
        .collect())
}

pub async fn codex_request_auth() -> anyhow::Result<CodexRequestAuth> {
    let cred = get_fresh_codex_credential().await?;
    Ok(CodexRequestAuth {
        access_token: cred.access,
        account_id: cred.account_id,
    })
}

async fn get_fresh_codex_credential() -> anyhow::Result<CodexOAuthCredential> {
    let path = auth_path();
    let data = load_auth_file(&path)?;
    let Some(StoredCredential::OAuth(cred)) = data.0.get(OPENAI_CODEX_PROVIDER).cloned() else {
        anyhow::bail!("no OpenAI Codex subscription auth found; run 'zerostack auth login codex'");
    };

    if !credential_needs_refresh(&cred) {
        return Ok(cred);
    }

    refresh_codex_credential_with_lock(&path).await
}

fn credential_needs_refresh(cred: &CodexOAuthCredential) -> bool {
    cred.expires <= now_millis().saturating_add(TOKEN_REFRESH_SKEW_MS)
}

async fn refresh_codex_credential_with_lock(path: &Path) -> anyhow::Result<CodexOAuthCredential> {
    let _lock = AuthFileLock::acquire(path)?;
    let mut data = load_auth_file(path)?;
    let Some(StoredCredential::OAuth(cred)) = data.0.get(OPENAI_CODEX_PROVIDER).cloned() else {
        anyhow::bail!("no OpenAI Codex subscription auth found; run 'zerostack auth login codex'");
    };

    // Another process may have refreshed or relogged in before we acquired the lock.
    if !credential_needs_refresh(&cred) {
        return Ok(cred);
    }

    let refreshed = refresh_openai_codex_token(&cred.refresh).await?;
    data.0.insert(
        OPENAI_CODEX_PROVIDER.to_string(),
        StoredCredential::OAuth(refreshed.clone()),
    );
    save_auth_file(path, &data)?;
    Ok(refreshed)
}

fn set_stored_credential(provider: &str, credential: StoredCredential) -> anyhow::Result<()> {
    let path = auth_path();
    let _lock = AuthFileLock::acquire(&path)?;
    let mut data = load_auth_file(&path)?;
    data.0.insert(provider.to_string(), credential);
    save_auth_file(&path, &data)
}

fn remove_stored_credential(provider: &str) -> anyhow::Result<()> {
    let path = auth_path();
    let _lock = AuthFileLock::acquire(&path)?;
    let mut data = load_auth_file(&path)?;
    data.0.remove(provider);
    save_auth_file(&path, &data)
}

fn load_auth_file(path: &Path) -> anyhow::Result<AuthFile> {
    if !path.exists() {
        return Ok(AuthFile::default());
    }
    let content = fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(AuthFile::default());
    }
    let map = serde_json::from_str::<HashMap<String, StoredCredential>>(&content)?;
    Ok(AuthFile(map))
}

fn save_auth_file(path: &Path, data: &AuthFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        set_private_dir(parent)?;
    }
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let json = serde_json::to_string_pretty(&data.0)?;
    fs::write(&tmp, format!("{json}\n"))?;
    set_private_file(&tmp)?;
    fs::rename(tmp, path)?;
    set_private_file(path)?;
    Ok(())
}

struct AuthFileLock {
    path: PathBuf,
}

impl AuthFileLock {
    fn acquire(auth_path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = auth_path.parent() {
            fs::create_dir_all(parent)?;
            set_private_dir(parent)?;
        }
        let lock_path = auth_path.with_extension("json.lock");
        for _ in 0..100 {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id())?;
                    return Ok(Self { path: lock_path });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(err) => return Err(err.into()),
            }
        }
        anyhow::bail!(
            "timed out waiting for auth file lock {}",
            lock_path.display()
        )
    }
}

impl Drop for AuthFileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn set_private_dir(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

async fn login_openai_codex_browser() -> anyhow::Result<CodexOAuthCredential> {
    let flow = create_authorization_flow();
    let (tx, rx) = mpsc::channel::<anyhow::Result<String>>();
    spawn_callback_listener(flow.state.clone(), tx.clone());
    spawn_manual_code_reader(flow.state.clone(), tx.clone());

    println!("OpenAI Codex browser login");
    println!(
        "Open this URL if your browser does not open automatically:\n{}",
        flow.url
    );
    println!("Waiting for browser callback, or paste the redirect URL/code here and press Enter.");
    let _ = open_url(&flow.url);

    let code = rx
        .recv_timeout(Duration::from_secs(15 * 60))
        .map_err(|_| anyhow::anyhow!("timed out waiting for OpenAI Codex login"))??;
    exchange_authorization_code(&code, &flow.verifier, OPENAI_CODEX_REDIRECT_URI).await
}

async fn login_openai_codex_device() -> anyhow::Result<CodexOAuthCredential> {
    let device = start_openai_codex_device_auth().await?;
    println!("OpenAI Codex device login");
    println!("Open: {}", OPENAI_CODEX_DEVICE_VERIFICATION_URI);
    println!("Code: {}", device.user_code);
    println!("Waiting for authentication...");
    let code = poll_openai_codex_device_auth(&device).await?;
    exchange_authorization_code(
        &code.authorization_code,
        &code.code_verifier,
        OPENAI_CODEX_DEVICE_REDIRECT_URI,
    )
    .await
}

struct AuthorizationFlow {
    verifier: String,
    state: String,
    url: String,
}

fn create_authorization_flow() -> AuthorizationFlow {
    let verifier = random_urlsafe(48);
    let challenge = BASE64_URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_urlsafe(16);
    let url = format!(
        "{OPENAI_CODEX_AUTHORIZE_URL}?{}",
        form_urlencoded(&[
            ("response_type", "code"),
            ("client_id", OPENAI_CODEX_CLIENT_ID),
            ("redirect_uri", OPENAI_CODEX_REDIRECT_URI),
            ("scope", OPENAI_CODEX_SCOPE),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "zerostack"),
        ])
    );
    AuthorizationFlow {
        verifier,
        state,
        url,
    }
}

fn random_urlsafe(bytes: usize) -> String {
    let mut raw = Vec::with_capacity(bytes);
    while raw.len() < bytes {
        raw.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    raw.truncate(bytes);
    BASE64_URL_SAFE_NO_PAD.encode(raw)
}

fn form_urlencoded(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", pct(k), pct(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn pct(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0xf) as usize] as char);
        }
    }
    out
}

fn spawn_callback_listener(state: String, tx: mpsc::Sender<anyhow::Result<String>>) {
    let Ok(listener) = TcpListener::bind("127.0.0.1:1455") else {
        return;
    };
    let _ = listener.set_nonblocking(true);
    std::thread::spawn(move || {
        let deadline = now_millis().saturating_add(15 * 60 * 1000);
        while now_millis() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = [0_u8; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    let result = parse_callback_request(&request, &state);
                    let body = if result.is_ok() {
                        "OpenAI authentication completed. You can close this window."
                    } else {
                        "OpenAI authentication failed. Return to the terminal."
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = tx.send(result);
                    return;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(err) => {
                    let _ = tx.send(Err(err.into()));
                    return;
                }
            }
        }
    });
}

fn parse_callback_request(request: &str, expected_state: &str) -> anyhow::Result<String> {
    let line = request.lines().next().unwrap_or_default();
    let path = line.split_whitespace().nth(1).unwrap_or_default();
    parse_authorization_input(path, expected_state)
}

fn spawn_manual_code_reader(state: String, tx: mpsc::Sender<anyhow::Result<String>>) {
    std::thread::spawn(move || {
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_ok() && !input.trim().is_empty() {
            let _ = tx.send(parse_authorization_input(&input, &state));
        }
    });
}

fn parse_authorization_input(input: &str, expected_state: &str) -> anyhow::Result<String> {
    let value = input.trim();
    if value.is_empty() {
        anyhow::bail!("missing authorization code");
    }
    let query = if let Some(idx) = value.find('?') {
        &value[idx + 1..]
    } else if let Some(idx) = value.find("code=") {
        &value[idx..]
    } else {
        return Ok(value.split('#').next().unwrap_or(value).to_string());
    };
    let params = parse_query_params(query);
    if let Some(state) = params.get("state")
        && state != expected_state
    {
        anyhow::bail!("state mismatch");
    }
    params
        .get("code")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing authorization code"))
}

fn parse_query_params(query: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        result.insert(percent_decode(key), percent_decode(value));
    }
    result
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push((hi << 4) | lo);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn open_url(url: &str) -> anyhow::Result<()> {
    let mut commands = Vec::new();
    if let Ok(browser) = std::env::var("BROWSER")
        && !browser.trim().is_empty()
    {
        commands.push(browser);
    }
    #[cfg(target_os = "macos")]
    commands.push("open".to_string());
    #[cfg(target_os = "linux")]
    commands.push("xdg-open".to_string());
    #[cfg(target_os = "windows")]
    commands.push("rundll32".to_string());

    for command in commands {
        let mut cmd = std::process::Command::new(&command);
        #[cfg(target_os = "windows")]
        if command == "rundll32" {
            cmd.arg("url.dll,FileProtocolHandler");
        }
        match cmd.arg(url).spawn() {
            Ok(_) => return Ok(()),
            Err(_) => continue,
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

async fn exchange_authorization_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> anyhow::Result<CodexOAuthCredential> {
    let response = reqwest::Client::new()
        .post(OPENAI_CODEX_TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(form_urlencoded(&[
            ("grant_type", "authorization_code"),
            ("client_id", OPENAI_CODEX_CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ]))
        .send()
        .await?;
    read_token_response(response, "exchange").await
}

async fn refresh_openai_codex_token(refresh_token: &str) -> anyhow::Result<CodexOAuthCredential> {
    let response = reqwest::Client::new()
        .post(OPENAI_CODEX_TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(form_urlencoded(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", OPENAI_CODEX_CLIENT_ID),
        ]))
        .send()
        .await?;
    read_token_response(response, "refresh").await
}

async fn read_token_response(
    response: reqwest::Response,
    operation: &str,
) -> anyhow::Result<CodexOAuthCredential> {
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI Codex token {operation} failed ({status}): {text}");
    }
    let token: TokenResponse = response.json().await?;
    credentials_from_token(token)
}

fn credentials_from_token(token: TokenResponse) -> anyhow::Result<CodexOAuthCredential> {
    let account_id = account_id_from_access_token(&token.access_token)?;
    Ok(CodexOAuthCredential {
        access: token.access_token,
        refresh: token.refresh_token,
        expires: now_millis().saturating_add(token.expires_in.saturating_mul(1000)),
        account_id,
    })
}

fn account_id_from_access_token(token: &str) -> anyhow::Result<String> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("invalid access token"))?;
    let decoded = BASE64_URL_SAFE_NO_PAD.decode(payload)?;
    let json: serde_json::Value = serde_json::from_slice(&decoded)?;
    json.get(OPENAI_CODEX_JWT_CLAIM_PATH)
        .and_then(|v| v.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow::anyhow!("failed to extract ChatGPT account id from access token"))
}

#[derive(Debug, Deserialize)]
struct DeviceStartResponse {
    device_auth_id: String,
    user_code: String,
    interval: serde_json::Value,
}

struct DeviceStart {
    device_auth_id: String,
    user_code: String,
    interval: Duration,
}

async fn start_openai_codex_device_auth() -> anyhow::Result<DeviceStart> {
    let response = reqwest::Client::new()
        .post(OPENAI_CODEX_DEVICE_USER_CODE_URL)
        .json(&serde_json::json!({ "client_id": OPENAI_CODEX_CLIENT_ID }))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI Codex device-code request failed ({status}): {text}");
    }
    let json: DeviceStartResponse = response.json().await?;
    let interval_seconds = match json.interval {
        serde_json::Value::Number(n) => n.as_u64().unwrap_or(5),
        serde_json::Value::String(s) => s.parse::<u64>().unwrap_or(5),
        _ => 5,
    };
    Ok(DeviceStart {
        device_auth_id: json.device_auth_id,
        user_code: json.user_code,
        interval: Duration::from_secs(interval_seconds.max(1)),
    })
}

struct DeviceCodeResult {
    authorization_code: String,
    code_verifier: String,
}

async fn poll_openai_codex_device_auth(device: &DeviceStart) -> anyhow::Result<DeviceCodeResult> {
    let client = reqwest::Client::new();
    let deadline = now_millis().saturating_add(15 * 60 * 1000);
    let mut interval = device.interval;
    while now_millis() < deadline {
        tokio::time::sleep(interval).await;
        let response = client
            .post(OPENAI_CODEX_DEVICE_TOKEN_URL)
            .json(&serde_json::json!({
                "device_auth_id": device.device_auth_id,
                "user_code": device.user_code,
            }))
            .send()
            .await?;
        if response.status().is_success() {
            let json: serde_json::Value = response.json().await?;
            let authorization_code = json
                .get("authorization_code")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!("device auth response missing authorization_code")
                })?;
            let code_verifier = json
                .get("code_verifier")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("device auth response missing code_verifier"))?;
            return Ok(DeviceCodeResult {
                authorization_code: authorization_code.to_string(),
                code_verifier: code_verifier.to_string(),
            });
        }
        if response.status().as_u16() == 403 || response.status().as_u16() == 404 {
            continue;
        }
        let text = response.text().await.unwrap_or_default();
        let code = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("code").or(Some(e)))
                    .and_then(|e| e.as_str())
                    .map(ToString::to_string)
            });
        match code.as_deref() {
            Some("deviceauth_authorization_pending") => continue,
            Some("slow_down") => {
                interval += Duration::from_secs(5);
                continue;
            }
            _ => anyhow::bail!("OpenAI Codex device auth failed: {text}"),
        }
    }
    anyhow::bail!("timed out waiting for OpenAI Codex device auth")
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn format_millis(value: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from(UNIX_EPOCH + Duration::from_millis(value))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod codex_auth_tests {
    use super::*;

    fn fake_jwt(account_id: &str) -> String {
        let payload = serde_json::json!({
            OPENAI_CODEX_JWT_CLAIM_PATH: {
                "chatgpt_account_id": account_id,
            }
        });
        format!(
            "header.{}.sig",
            BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        )
    }

    #[test]
    fn extracts_codex_account_id_from_access_token() {
        assert_eq!(
            account_id_from_access_token(&fake_jwt("acct_123")).unwrap(),
            "acct_123"
        );
    }

    #[test]
    fn parses_authorization_redirect_url() {
        let parsed = parse_authorization_input(
            "http://localhost:1455/auth/callback?code=abc%2F123&state=state-1",
            "state-1",
        )
        .unwrap();
        assert_eq!(parsed, "abc/123");
    }

    #[test]
    fn auth_file_roundtrips_pi_compatible_oauth_record() {
        let dir = std::env::temp_dir().join(format!(
            "zerostack-auth-test-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        let mut data = AuthFile::default();
        data.0.insert(
            OPENAI_CODEX_PROVIDER.to_string(),
            StoredCredential::OAuth(CodexOAuthCredential {
                access: "access".to_string(),
                refresh: "refresh".to_string(),
                expires: 42,
                account_id: "acct".to_string(),
            }),
        );
        save_auth_file(&path, &data).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"type\": \"oauth\""));
        assert!(raw.contains("\"accountId\": \"acct\""));
        let loaded = load_auth_file(&path).unwrap();
        assert_eq!(loaded.0, data.0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn auth_file_filters_api_keys_for_resolver() {
        let dir = std::env::temp_dir().join(format!(
            "zerostack-auth-test-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        let mut data = AuthFile::default();
        data.0.insert(
            "openai".to_string(),
            StoredCredential::ApiKey {
                key: "sk-test".to_string(),
            },
        );
        data.0.insert(
            OPENAI_CODEX_PROVIDER.to_string(),
            StoredCredential::OAuth(CodexOAuthCredential {
                access: "access".to_string(),
                refresh: "refresh".to_string(),
                expires: 42,
                account_id: "acct".to_string(),
            }),
        );
        save_auth_file(&path, &data).unwrap();
        let loaded = api_keys_from_auth_file(&path).unwrap();
        assert_eq!(loaded.get("openai").map(String::as_str), Some("sk-test"));
        assert!(!loaded.contains_key(OPENAI_CODEX_PROVIDER));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn provider_aliases_normalize_to_openai_codex() {
        assert_eq!(
            normalize_auth_provider("codex").unwrap(),
            OPENAI_CODEX_PROVIDER
        );
        assert_eq!(
            normalize_auth_provider("openai-codex").unwrap(),
            OPENAI_CODEX_PROVIDER
        );
        assert!(normalize_auth_provider("openai").is_err());
    }

    #[test]
    fn credential_provider_key_normalizes_known_providers_and_keeps_custom_names() {
        assert_eq!(credential_provider_key("google").unwrap(), "gemini");
        assert_eq!(
            credential_provider_key("codex").unwrap(),
            OPENAI_CODEX_PROVIDER
        );
        assert_eq!(credential_provider_key("local-vllm").unwrap(), "local-vllm");
        assert!(credential_provider_key(" ").is_err());
    }
}
