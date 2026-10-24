use super::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

async fn server(
    responses: Vec<(u16, Value)>,
) -> (String, tokio::task::JoinHandle<Vec<(String, Value)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(15), async move {
            let mut requests = Vec::new();
            for (status, response) in responses {
                let (socket, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(socket);
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).await.unwrap() > 0);
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    { length = value.trim().parse::<usize>().unwrap(); }
                    headers.push_str(&line);
                    if line == "\r\n" { break; }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.unwrap();
                requests.push((headers.to_ascii_lowercase(), if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap() }));
                let body = response.to_string();
                reader.get_mut().write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            requests
        }).await.unwrap()
    });
    (url, task)
}

fn token_response() -> Value {
    json!({"access_token":"new-access", "refresh_token":"new-refresh", "expires_in":3600})
}

fn device() -> Device {
    Device {
        device_code: "device-secret".into(),
        user_code: "USER-CODE".into(),
        verification_uri_complete: "https://opencode.ai/console/device".into(),
        expires_in: 30,
        interval: 1,
    }
}

#[tokio::test]
async fn device_polling_handles_pending_and_slow_down() {
    let (url, task) = server(vec![
        (400, json!({"error":"authorization_pending"})),
        (400, json!({"error":"slow_down"})),
        (200, token_response()),
    ])
    .await;
    let start = std::time::Instant::now();
    let result = poll(&client().unwrap(), &url, &device()).await.unwrap();
    assert_eq!(result.access_token, "new-access");
    assert!(start.elapsed() >= Duration::from_secs(8));
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 3);
    for (headers, body) in requests {
        assert!(headers.starts_with("post /auth/device/token "));
        assert!(headers.contains("content-type: application/json"));
        assert!(headers.contains("user-agent: zerostack/"));
        assert_eq!(
            body,
            json!({"grant_type":"urn:ietf:params:oauth:grant-type:device_code", "device_code":"device-secret", "client_id":"opencode-cli"})
        );
    }
}

#[tokio::test]
async fn device_polling_stops_on_denial_and_deadline() {
    let (url, task) = server(vec![(400, json!({"error":"access_denied"}))]).await;
    assert!(
        poll(&client().unwrap(), &url, &device())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("denied")
    );
    task.await.unwrap();
    let mut expired = device();
    expired.expires_in = 0;
    assert!(
        poll(&client().unwrap(), "http://127.0.0.1:1", &expired)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("timed out")
    );
}

struct AuthStore(PathBuf);
impl AuthStore {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .join(format!("zerostack-opencode-auth-{}", Uuid::new_v4()))
            .join("auth.json");
        let mut data = AuthFile::default();
        data.0.insert(
            PROVIDER.into(),
            StoredCredential::OpenCodeOAuth(Credential {
                access: "old-access".into(),
                refresh: "old-refresh".into(),
                expires: 0,
                account_id: "account".into(),
                org_id: Some("org-original".into()),
            }),
        );
        data.0.insert(
            "unrelated".into(),
            StoredCredential::ApiKey {
                key: "keep-me".into(),
            },
        );
        save_auth_file(&path, &data).unwrap();
        Self(path)
    }
}
impl Drop for AuthStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[tokio::test]
async fn refresh_rotates_once_for_concurrent_requests_and_preserves_other_credentials() {
    let store = AuthStore::new();
    let (url, task) = server(vec![(200, token_response())]).await;
    let (first, second) = tokio::join!(fresh(&store.0, &url), fresh(&store.0, &url));
    let cred = first.unwrap();
    assert_eq!(second.unwrap(), cred);
    assert_eq!(cred.access, "new-access");
    assert_eq!(cred.refresh, "new-refresh");
    assert_eq!(cred.org_id.as_deref(), Some("org-original"));
    assert!(cred.expires > now_millis() + TOKEN_REFRESH_SKEW_MS);
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].1,
        json!({"grant_type":"refresh_token", "refresh_token":"old-refresh", "client_id":"opencode-cli"})
    );
    let data = load_auth_file(&store.0).unwrap();
    assert_eq!(
        data.0[PROVIDER],
        StoredCredential::OpenCodeOAuth(cred.clone())
    );
    assert_eq!(
        data.0["unrelated"],
        StoredCredential::ApiKey {
            key: "keep-me".into()
        }
    );
    // A fresh credential must work without contacting the token endpoint.
    assert_eq!(fresh(&store.0, "http://127.0.0.1:1").await.unwrap(), cred);
    assert!(
        !api_keys_from_auth_file(&store.0)
            .unwrap()
            .contains_key(PROVIDER)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&store.0).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn failed_refresh_preserves_credentials_and_releases_lock() {
    let store = AuthStore::new();
    let before = fs::read(&store.0).unwrap();
    let (url, task) = server(vec![(
        400,
        json!({"error":"invalid_grant", "secret":"must-not-print"}),
    )])
    .await;
    let error = fresh(&store.0, &url).await.unwrap_err().to_string();
    assert!(error.contains("zerostack auth login opencode-go"));
    assert!(!error.contains("must-not-print"));
    assert_eq!(fs::read(&store.0).unwrap(), before);
    assert!(!store.0.with_extension("json.lock").exists());
    task.await.unwrap();
}

#[test]
fn organization_selection_is_deterministic_and_scoped_tokens_must_match() {
    let user = json!({"id":"account"});
    let orgs = vec![json!({"id":"z", "name":"Z"}), json!({"id":"a", "name":"A"})];
    let cred = credential(token(token_response()).unwrap(), &user, orgs.clone()).unwrap();
    assert_eq!(cred.org_id.as_deref(), Some("a"));
    let mut value = token_response();
    value["org_id"] = json!("missing");
    assert!(credential(token(value).unwrap(), &user, orgs).is_err());
}

#[test]
fn console_routes_support_model_overrides_and_all_three_protocols() {
    let config = json!({"providers":{"opencode-go":{
        "settings":{"baseURL":"https://opencode.ai/console/inference/v1"},
        "models":{
            "chat":{"package":"@opencode/ai/providers/openai-compatible"},
            "response":{"package":"@opencode/ai/providers/openai"},
            "messages":{"package":"aisdk:@ai-sdk/anthropic", "settings":{"baseURL":"https://opencode.ai/console/anthropic/v1"}}
        }
    }}});
    assert!(
        route(&config, "chat", "chat/completions")
            .unwrap()
            .0
            .ends_with("/v1/chat/completions")
    );
    assert!(
        route(&config, "response", "responses")
            .unwrap()
            .0
            .ends_with("/v1/responses")
    );
    assert!(
        route(&config, "messages", "v1/messages")
            .unwrap()
            .0
            .ends_with("/anthropic/v1/messages")
    );
    assert!(route(&config, "messages", "chat/completions").is_err());
}

#[tokio::test]
async fn console_config_uses_bearer_and_org_scope_and_rejects_unenforceable_policies() {
    let cred = Credential {
        access: "access".into(),
        refresh: "refresh".into(),
        expires: u64::MAX,
        account_id: "account".into(),
        org_id: Some("org".into()),
    };
    for (config, allowed) in [
        (json!({"providers":{},"policies":[]}), true),
        (
            json!({"providers":{},"policies":[{"effect":"deny"}]}),
            false,
        ),
        (
            json!({"providers":{},"experimental":{"policies":[{}]}}),
            false,
        ),
        (json!({"providers":{},"policies":"invalid"}), false),
    ] {
        let (url, task) = server(vec![(200, config)]).await;
        assert_eq!(config_at(&cred, &url).await.is_ok(), allowed);
        let requests = task.await.unwrap();
        assert!(requests[0].0.starts_with("get /api/v2/config "));
        assert!(requests[0].0.contains("authorization: bearer access"));
        assert!(requests[0].0.contains("x-org-id: org"));
    }
    let (url, task) = server(vec![(401, json!({"secret":"do-not-echo"}))]).await;
    let error = config_at(&cred, &url).await.unwrap_err().to_string();
    assert!(error.contains("login opencode-go"));
    assert!(!error.contains("do-not-echo"));
    task.await.unwrap();
}

#[test]
fn opencode_auth_aliases_normalize_consistently() {
    for provider in ["opencode", "opencode-go", "OPENCODE"] {
        assert_eq!(normalize_auth_provider(provider).unwrap(), PROVIDER);
        assert_eq!(credential_provider_key(provider).unwrap(), PROVIDER);
    }
}

#[test]
fn verification_urls_resolve_relative_paths_and_reject_untrusted_origins() {
    assert_eq!(
        verification_url("auth/device?code=123").unwrap().as_str(),
        "https://opencode.ai/console/auth/device?code=123"
    );
    for value in [
        "http://opencode.ai/device",
        "https://evil.example/device",
        "https://opencode.ai:8443/device",
        "https://user@opencode.ai/device",
        "//evil.example/device",
    ] {
        assert!(verification_url(value).is_err(), "{value}");
    }
}

#[test]
fn cli_accepts_opencode_oauth_commands() {
    use crate::cli::{AuthCommand, Cli, Command};
    use clap::Parser;
    for args in [
        vec!["zerostack", "auth", "login", "opencode-go"],
        vec!["zerostack", "auth", "login", "opencode-go", "--device"],
    ] {
        assert!(matches!(Cli::try_parse_from(args).unwrap().command,
            Some(Command::Auth { command: AuthCommand::Login { provider, .. } }) if provider == PROVIDER));
    }
    assert!(Cli::try_parse_from(["zerostack", "auth", "logout", "opencode-go"]).is_ok());
    assert!(Cli::try_parse_from(["zerostack", "auth", "status", "opencode-go"]).is_ok());
}

#[test]
fn console_overlay_keeps_default_go_routes_and_never_uses_zen_overrides() {
    // Shape returned by the live Console: Go has no model/URL overrides.
    let config = json!({"providers":{
        "opencode":{"settings":{"baseURL":"https://opencode.ai/zen/v1"},"models":{"deepseek-v4.1-flash":{"disabled":true}}},
        "opencode-go":{"headers":{"x-opencode-org-id":"workspace"},"models":{}}
    }});
    assert_eq!(
        route(&config, "deepseek-v4.1-flash", "chat/completions").unwrap(),
        (
            "https://opencode.ai/zen/go/v1/chat/completions".into(),
            "deepseek-v4.1-flash".into()
        )
    );
    assert_eq!(
        route(&config, "minimax-m3", "v1/messages").unwrap().0,
        "https://opencode.ai/zen/go/v1/messages"
    );
    assert_eq!(
        route(&config, "gpt-6-luna", "responses").unwrap().0,
        "https://opencode.ai/zen/go/v1/responses"
    );
    let mut absent_go = config;
    absent_go["providers"]
        .as_object_mut()
        .unwrap()
        .remove("opencode-go");
    assert_eq!(
        route(&absent_go, "deepseek-v4.1-flash", "chat/completions")
            .unwrap()
            .0,
        "https://opencode.ai/zen/go/v1/chat/completions"
    );
    assert!(route(&json!({}), "deepseek-v4.1-flash", "chat/completions").is_err());
}
