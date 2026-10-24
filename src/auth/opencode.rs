//! OpenCode Console device authorization and request-time OAuth credentials.
use super::*;
use serde_json::{Value, json};

const SERVER: &str = "https://opencode.ai/console";
const CLIENT_ID: &str = "opencode-cli";
pub const PROVIDER: &str = "opencode-go";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credential {
    pub access: String,
    pub refresh: String,
    pub expires: u64,
    pub account_id: String,
    pub org_id: Option<String>,
}

// Do not expose tokens through debug output.
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCodeOAuthCredential")
            .field("expires", &self.expires)
            .field("account_id", &self.account_id)
            .field("org_id", &self.org_id)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct Device {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    org_id: Option<String>,
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("zerostack/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?)
}

async fn post(
    client: &reqwest::Client,
    server: &str,
    path: &str,
    body: Value,
) -> anyhow::Result<Value> {
    let response = client
        .post(format!("{server}{path}"))
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let value: Value = response.json().await?;
    // Device polling uses 400 responses for pending and slow_down. Never include response
    // bodies in errors: they can contain credentials.
    if !status.is_success() && value.get("error").and_then(Value::as_str).is_none() {
        anyhow::bail!("OpenCode Console request failed ({status})");
    }
    Ok(value)
}

fn token(value: Value) -> anyhow::Result<Token> {
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        if error == "invalid_grant" || error == "access_denied" || error == "expired_token" {
            anyhow::bail!(
                "OpenCode authorization expired or was denied; run 'zerostack auth login opencode-go'"
            );
        }
        anyhow::bail!("OpenCode authorization failed; run 'zerostack auth login opencode-go'");
    }
    let token: Token = serde_json::from_value(value)?;
    anyhow::ensure!(
        !token.access_token.is_empty() && !token.refresh_token.is_empty() && token.expires_in > 0,
        "OpenCode returned invalid tokens"
    );
    Ok(token)
}

async fn poll(client: &reqwest::Client, server: &str, device: &Device) -> anyhow::Result<Token> {
    let mut interval = device.interval.max(1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(device.expires_in.min(1800));
    tokio::time::timeout_at(deadline, async {
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let value = post(
                client,
                server,
                "/auth/device/token",
                json!({
                    "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                    "device_code": device.device_code,
                    "client_id": CLIENT_ID,
                }),
            )
            .await?;
            match value.get("error").and_then(Value::as_str) {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    interval = interval.saturating_add(5);
                }
                _ => return token(value),
            }
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "OpenCode device login timed out; run 'zerostack auth login opencode-go' again"
        )
    })?
}

fn verification_url(value: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(&format!("{SERVER}/"))?.join(value)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("opencode.ai")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none(),
        "OpenCode returned an untrusted verification URL"
    );
    Ok(url)
}
pub(super) async fn login() -> anyhow::Result<Credential> {
    let http = client()?;
    let value = post(
        &http,
        SERVER,
        "/auth/device/code",
        json!({
            "client_id": CLIENT_ID, "supports_org_scope": true,
        }),
    )
    .await?;
    let device: Device = serde_json::from_value(value)?;
    let url = verification_url(&device.verification_uri_complete)?;
    println!(
        "OpenCode Console device login\nOpen: {url}\nCode: {}\nWaiting for authorization...",
        device.user_code
    );
    let _ = open_url(url.as_str());
    let token = poll(&http, SERVER, &device).await?;
    let user: Value = http
        .get(format!("{SERVER}/api/user"))
        .bearer_auth(&token.access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let orgs: Vec<Value> = http
        .get(format!("{SERVER}/api/orgs"))
        .bearer_auth(&token.access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    credential(token, &user, orgs)
}

fn credential(token: Token, user: &Value, mut orgs: Vec<Value>) -> anyhow::Result<Credential> {
    orgs.sort_by(|a, b| {
        a["name"]
            .as_str()
            .cmp(&b["name"].as_str())
            .then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let org = match token.org_id.as_deref() {
        Some(id) => Some(
            orgs.iter()
                .find(|org| org["id"].as_str() == Some(id))
                .ok_or_else(|| {
                    anyhow::anyhow!("OpenCode authorization returned an unknown organization")
                })?,
        ),
        None => orgs.first(),
    };
    Ok(Credential {
        access: token.access_token,
        refresh: token.refresh_token,
        expires: now_millis().saturating_add(token.expires_in.saturating_mul(1000)),
        account_id: user["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("OpenCode returned no account ID"))?
            .to_string(),
        org_id: org.and_then(|org| org["id"].as_str()).map(str::to_string),
    })
}

pub fn configured() -> anyhow::Result<bool> {
    Ok(matches!(
        load_auth_file(&auth_path())?.0.get(PROVIDER),
        Some(StoredCredential::OpenCodeOAuth(_))
    ))
}

pub async fn request_auth() -> anyhow::Result<Credential> {
    fresh(&auth_path(), SERVER).await
}

async fn fresh(path: &Path, server: &str) -> anyhow::Result<Credential> {
    fn read(path: &Path) -> anyhow::Result<Credential> {
        match load_auth_file(path)?.0.remove(PROVIDER) {
            Some(StoredCredential::OpenCodeOAuth(cred)) => Ok(cred),
            _ => anyhow::bail!(
                "no OpenCode OAuth credentials; run 'zerostack auth login opencode-go'"
            ),
        }
    }
    let cred = read(path)?;
    if cred.expires > now_millis().saturating_add(TOKEN_REFRESH_SKEW_MS) {
        return Ok(cred);
    }
    // Do not block the async runtime while another process refreshes the token.
    let lock_path = path.to_path_buf();
    // Allow time for another process's bounded token request to finish.
    let _lock =
        tokio::task::spawn_blocking(move || AuthFileLock::acquire_with_attempts(&lock_path, 1750))
            .await??;
    let mut cred = read(path)?;
    if cred.expires > now_millis().saturating_add(TOKEN_REFRESH_SKEW_MS) {
        return Ok(cred);
    }
    let next = token(post(&client()?, server, "/auth/device/token", json!({
        "grant_type": "refresh_token", "refresh_token": cred.refresh, "client_id": CLIENT_ID,
    })).await?)?;
    cred.access = next.access_token;
    cred.refresh = next.refresh_token;
    cred.expires = now_millis().saturating_add(next.expires_in.saturating_mul(1000));
    if next.org_id.is_some() {
        cred.org_id = next.org_id;
    }
    let mut data = load_auth_file(path)?;
    data.0.insert(
        PROVIDER.to_string(),
        StoredCredential::OpenCodeOAuth(cred.clone()),
    );
    save_auth_file(path, &data)?;
    Ok(cred)
}

pub async fn config(cred: &Credential) -> anyhow::Result<Value> {
    config_at(cred, SERVER).await
}

async fn config_at(cred: &Credential, server: &str) -> anyhow::Result<Value> {
    let mut request = client()?
        .get(format!("{server}/api/v2/config"))
        .bearer_auth(&cred.access);
    if let Some(org) = &cred.org_id {
        request = request.header("x-org-id", org);
    }
    let response = request.send().await?;
    anyhow::ensure!(
        response.status().is_success(),
        "OpenCode Console config failed ({}); check your subscription or run 'zerostack auth login opencode-go'",
        response.status()
    );
    let config: Value = response.json().await?;
    // Do not silently bypass organization policy that this client cannot enforce.
    for policies in [
        config.get("policies"),
        config.pointer("/experimental/policies"),
    ] {
        anyhow::ensure!(
            policies.is_none_or(|v| v.as_array().is_some_and(Vec::is_empty)),
            "OpenCode organization policies are not supported by zerostack"
        );
    }
    Ok(config)
}

pub fn provider_config(config: &Value) -> anyhow::Result<&Value> {
    let providers = config["providers"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("OpenCode Console returned no providers"))?;
    // Console config overlays the normal provider catalog. A missing provider
    // or model entry means "unchanged", not "unavailable". Zen is a separate
    // provider: never apply its model restrictions or routes to Go.
    Ok(providers.get(PROVIDER).unwrap_or(&Value::Null))
}

/// Apply Console overrides to the normal Go route. Explicitly disabled models stay disabled.
pub fn route(config: &Value, model: &str, endpoint: &str) -> anyhow::Result<(String, String)> {
    let provider = provider_config(config)?;
    let entry = &provider["models"][model];
    anyhow::ensure!(
        provider["disabled"] != true && entry["disabled"] != true,
        "OpenCode Console does not offer model '{model}'"
    );
    if let Some(package) = entry
        .get("package")
        .or_else(|| provider.get("package"))
        .and_then(Value::as_str)
    {
        let compatible = match endpoint {
            "chat/completions" => {
                package.contains("openai-compatible") || package.ends_with("/openai")
            }
            "responses" => package.ends_with("/openai") || package.contains("responses"),
            "v1/messages" => package.contains("anthropic"),
            _ => false,
        };
        anyhow::ensure!(
            compatible,
            "OpenCode model '{model}' requires a different API protocol; this model is not yet supported by zerostack"
        );
    }
    let base = entry
        .pointer("/settings/baseURL")
        .or_else(|| provider.pointer("/settings/baseURL"))
        .and_then(Value::as_str)
        .unwrap_or(crate::provider::OPENCODE_GO_BASE_URL);
    let url = reqwest::Url::parse(base)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("opencode.ai")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "OpenCode Console returned an untrusted inference URL"
    );
    let id = entry["modelID"].as_str().unwrap_or(model);
    // Rig appends /v1/messages for Anthropic, while Console base URLs already include it.
    let endpoint = if endpoint == "v1/messages" && base.trim_end_matches('/').ends_with("/v1") {
        "messages"
    } else {
        endpoint
    };
    Ok((
        format!("{}/{endpoint}", base.trim_end_matches('/')),
        id.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_authorized_org_and_keeps_tokens_out_of_debug() {
        let token = token(json!({"access_token":"secret-access", "refresh_token":"secret-refresh", "expires_in":3600, "org_id":"b"})).unwrap();
        let cred = credential(
            token,
            &json!({"id":"user"}),
            vec![json!({"id":"a","name":"A"}), json!({"id":"b","name":"B"})],
        )
        .unwrap();
        assert_eq!(cred.org_id.as_deref(), Some("b"));
        assert!(!format!("{cred:?}").contains("secret"));
        assert!(cred.expires > now_millis());
    }

    #[test]
    fn rejects_invalid_tokens() {
        for value in [
            json!({"error":"invalid_grant"}),
            json!({"access_token":"", "refresh_token":"r", "expires_in":1}),
        ] {
            assert!(token(value).is_err());
        }
    }

    #[test]
    fn resolves_console_routes_and_rejects_disabled_or_untrusted_models() {
        let mut config = json!({"providers":{"opencode-go":{"settings":{"baseURL":"https://opencode.ai/console/inference/v1"},"models":{"glm":{"modelID":"glm-upstream"}}}}});
        assert_eq!(
            route(&config, "glm", "chat/completions").unwrap(),
            (
                "https://opencode.ai/console/inference/v1/chat/completions".into(),
                "glm-upstream".into()
            )
        );
        assert!(route(&config, "missing", "chat/completions").is_ok());
        config["providers"]["opencode-go"]["models"]["glm"]["disabled"] = json!(true);
        assert!(route(&config, "glm", "chat/completions").is_err());
        config["providers"]["opencode-go"]["models"]["glm"]["disabled"] = json!(false);
        config["providers"]["opencode-go"]["settings"]["baseURL"] =
            json!("https://evil.example/v1");
        assert!(route(&config, "glm", "chat/completions").is_err());
    }
}

#[cfg(test)]
mod protocol_tests;
