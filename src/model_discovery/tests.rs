use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("zerostack-model-cache-{}", uuid::Uuid::new_v4())))
    }
    fn cache(&self) -> Cache {
        Cache::new(self.0.clone())
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn model(id: &str) -> ModelEntry {
    ModelEntry {
        id: id.into(),
        display: id.into(),
        context_length: Some(12345),
        kind: None,
    }
}

#[test]
fn scopes_separate_provider_endpoint_account_workspace_keys_and_headers_without_leaking_secrets() {
    let headers = BTreeMap::new();
    let key = scope_key(
        "provider",
        Some("https://gateway.example/v1"),
        "secret-api-key",
        &headers,
    );
    assert_eq!(key.len(), 64);
    assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!key.contains("secret"));
    assert_eq!(
        key,
        scope_key(
            "provider",
            Some("https://gateway.example/v1"),
            "secret-api-key",
            &headers
        )
    );
    for other in [
        scope_key(
            "other-provider",
            Some("https://gateway.example/v1"),
            "secret-api-key",
            &headers,
        ),
        scope_key(
            "provider",
            Some("https://other-gateway.example/v1"),
            "secret-api-key",
            &headers,
        ),
        scope_key(
            "provider",
            Some("https://gateway.example/v1"),
            "other-key",
            &headers,
        ),
        scope_key(
            "provider",
            Some("https://gateway.example/v1"),
            "secret-api-key",
            &BTreeMap::from([("x-workspace".into(), "other-workspace".into())]),
        ),
    ] {
        assert_ne!(key, other);
    }
    let account =
        |account, workspace| serde_json::to_string(&("oauth", account, workspace)).unwrap();
    assert_ne!(
        scope_key("go", None, &account("a", "org"), &headers),
        scope_key("go", None, &account("b", "org"), &headers)
    );
    assert_ne!(
        scope_key("go", None, &account("a", "org"), &headers),
        scope_key("go", None, &account("a", "other"), &headers)
    );
}

#[tokio::test]
async fn persists_successful_lists_and_notifies_subscribers() {
    let directory = TestDirectory::new();
    let cache = directory.cache();
    let mut updates = cache.updates.subscribe();
    assert!(cache.snapshot("scope").is_none());
    let models = cache
        .refresh("scope", false, || async { Ok(vec![model("new-model")]) })
        .await
        .unwrap();
    assert_eq!(models[0].id, "new-model");
    assert!(updates.has_changed().unwrap());
    updates.borrow_and_update();
    let second_process = directory.cache();
    let record = second_process.snapshot("scope").unwrap();
    assert_eq!(record.models[0].id, "new-model");
    assert_eq!(record.models[0].context_length, Some(12345));
    assert!(!second_process.reserve("scope"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(directory.0.join("scope.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn failed_refresh_preserves_memory_and_disk_and_applies_retry_cooldown() {
    let directory = TestDirectory::new();
    let cache = directory.cache();
    cache
        .refresh("scope", false, || async { Ok(vec![model("last-success")]) })
        .await
        .unwrap();
    let before = std::fs::read(directory.0.join("scope.json")).unwrap();
    assert!(
        cache
            .refresh("scope", true, || async {
                anyhow::bail!("provider unavailable")
            })
            .await
            .is_err()
    );
    assert_eq!(
        cache.snapshot("scope").unwrap().models[0].id,
        "last-success"
    );
    assert_eq!(
        std::fs::read(directory.0.join("scope.json")).unwrap(),
        before
    );
    cache
        .slots
        .lock()
        .unwrap()
        .get_mut("scope")
        .unwrap()
        .record
        .as_mut()
        .unwrap()
        .fetched_at = 0;
    assert!(!cache.reserve("scope"));
    assert!(cache.snapshot("other-account").is_none());
}

#[tokio::test]
async fn empty_success_replaces_stale_models_without_filling_from_static_catalog() {
    let directory = TestDirectory::new();
    let cache = directory.cache();
    cache
        .refresh("scope", false, || async { Ok(vec![model("retired")]) })
        .await
        .unwrap();
    cache
        .refresh("scope", true, || async { Ok(Vec::new()) })
        .await
        .unwrap();
    assert!(cache.snapshot("scope").unwrap().models.is_empty());
    assert!(
        directory
            .cache()
            .snapshot("scope")
            .unwrap()
            .models
            .is_empty()
    );
}

#[tokio::test]
async fn automatic_refreshes_deduplicate_and_cold_reads_do_not_wait() {
    let directory = TestDirectory::new();
    let cache = directory.cache();
    assert!(cache.reserve("scope"));
    assert!(!cache.reserve("scope"));
    let calls = AtomicUsize::new(0);
    let fetch = || async {
        calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(vec![model("live")])
    };
    let (first, second) = tokio::join!(
        cache.refresh("scope", false, fetch),
        cache.refresh("scope", false, fetch)
    );
    assert_eq!(first.unwrap()[0].id, "live");
    assert_eq!(second.unwrap()[0].id, "live");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn corrupt_future_and_unknown_version_cache_records_are_ignored() {
    let directory = TestDirectory::new();
    std::fs::create_dir_all(&directory.0).unwrap();
    let path = directory.0.join("scope.json");
    std::fs::write(&path, "not JSON").unwrap();
    assert!(read_record(&path).is_none());
    for (version, fetched_at, models) in [
        (999, now(), vec![model("m")]),
        (1, now() + 3600, vec![model("m")]),
        (1, now(), vec![model("")]),
    ] {
        write_record(
            &directory.0,
            "scope",
            &Record {
                version,
                fetched_at,
                models,
            },
        )
        .unwrap();
        assert!(read_record(&path).is_none());
    }
}

#[tokio::test]
async fn live_custom_discovery_filters_non_agent_models_and_never_adds_catalog_entries() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(socket);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).await.unwrap() > 0);
            headers.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let body = r#"{"data":[{"id":"new-model","name":"Live model","context_length":77777},{"id":"text-embedding-small"},{"id":"new-model"}]}"#;
        reader.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        headers
    });
    let cfg = Config { custom_providers: Some(HashMap::from([("test-gateway".into(), serde_json::from_value(serde_json::json!({"provider_type":"openai", "base_url":base, "headers":{"x-workspace":"test"}})).unwrap())])), ..Config::default() };
    let query = Query::new("test-gateway", Some("test-key"), &cfg).unwrap();
    let models = query.fetch().await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "new-model");
    assert_eq!(models[0].context_length, Some(77777));
    assert_eq!(models[0].display, "Live model");
    let headers = server.await.unwrap().to_ascii_lowercase();
    assert!(headers.contains("authorization: bearer test-key"));
    assert!(headers.contains("x-workspace: test"));
}

#[test]
fn metadata_only_enriches_returned_models_and_invalid_ids_fail() {
    let mut known = model("gpt-5.5");
    known.context_length = None;
    let models = prepare_models(
        "openai",
        vec![
            known,
            model("new-model"),
            model("new-model"),
            model("text-embedding-small"),
        ],
    )
    .unwrap();
    assert_eq!(models.len(), 2);
    assert!(
        models
            .iter()
            .find(|model| model.id == "gpt-5.5")
            .unwrap()
            .context_length
            .is_some()
    );
    assert_eq!(
        models
            .iter()
            .find(|model| model.id == "new-model")
            .unwrap()
            .context_length,
        Some(12345)
    );
    for id in ["", "  ", "bad\nmodel", "bad\u{1b}model"] {
        assert!(prepare_models("openai", vec![model(id)]).is_err());
    }
}

#[test]
fn cli_model_refresh_flag_is_available() {
    use clap::Parser;
    let cli = crate::cli::Cli::try_parse_from([
        "zerostack",
        "config",
        "models",
        "opencode-go",
        "--refresh",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Some(crate::cli::Command::Config {
            command: crate::cli::ConfigCommand::Models { refresh: true, .. }
        })
    ));
}

#[test]
fn codex_cache_separates_users_and_workspaces_but_survives_token_rotation() {
    let token = |user: &str, signature: &str| {
        format!(
            "header.{}.{}",
            base64::prelude::BASE64_URL_SAFE_NO_PAD
                .encode(serde_json::json!({"sub": user}).to_string()),
            signature
        )
    };
    let first = codex_identity("workspace-a", &token("user-a", "first")).unwrap();
    assert_eq!(
        first,
        codex_identity("workspace-a", &token("user-a", "rotated")).unwrap()
    );
    assert_ne!(
        first,
        codex_identity("workspace-a", &token("user-b", "first")).unwrap()
    );
    assert_ne!(
        first,
        codex_identity("workspace-b", &token("user-a", "first")).unwrap()
    );
    assert_ne!(
        codex_identity("workspace-a", "opaque-a").unwrap(),
        codex_identity("workspace-a", "opaque-b").unwrap()
    );
}
