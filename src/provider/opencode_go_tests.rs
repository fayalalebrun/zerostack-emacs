use super::*;
use rig::completion::Message;
use rig::error::ProviderError;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::{Duration, timeout};

async fn capture_request() -> (OpenCodeGoClient, tokio::task::JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/zen/go/v1/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        timeout(Duration::from_secs(10), async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).await.unwrap();
            reader.get_mut().write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 14\r\nConnection: close\r\n\r\nmock rejection",
            ).await.unwrap();
            (headers.to_ascii_lowercase(), if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap() })
        }).await.unwrap()
    });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-test-header", "preserved".parse().unwrap());
    let http = reqwest::Client::builder()
        .no_proxy()
        .default_headers(headers)
        .build()
        .unwrap();
    (
        build_opencode_go_client("test-key", Some(&base), http, Some("test-session")).unwrap(),
        task,
    )
}

#[test]
fn opencode_go_session_identity_survives_cloning_and_client_recreation() {
    for id in ["conversation-a", "conversation-b"] {
        for _ in 0..2 {
            let AnyClient::OpenCodeGo(client) = create_client(
                "opencode-go",
                Some("test-key"),
                &HashMap::new(),
                None,
                Some(id),
            )
            .unwrap() else {
                panic!("Go client")
            };
            for client in [client.clone(), client] {
                for headers in [&client.headers, &client.headers, &client.headers] {
                    assert_eq!(headers["x-opencode-session"], id);
                    assert_eq!(
                        headers["user-agent"],
                        concat!("zerostack/", env!("CARGO_PKG_VERSION"))
                    );
                }
            }
        }
    }
}

#[test]
fn opencode_go_missing_session_gets_one_stable_client_id() {
    let mut ids = std::collections::HashSet::new();
    for input in [None, Some(""), Some(" ")] {
        let client =
            build_opencode_go_client("test-key", None, reqwest::Client::new(), input).unwrap();
        let id = client.headers["x-opencode-session"].to_str().unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok());
        assert!(ids.insert(id.to_string()));
        let cloned = client.clone();
        for headers in [&cloned.headers, &cloned.headers, &cloned.headers] {
            assert_eq!(headers["x-opencode-session"], id);
        }
    }
}

#[test]
fn opencode_go_rejects_session_header_injection() {
    assert!(
        build_opencode_go_client(
            "test-key",
            None,
            reqwest::Client::new(),
            Some("session\r\nx-injected: true"),
        )
        .is_err()
    );
}

async fn send_request(
    model: rig::DynModel<rig::operation::Completion>,
    params: Option<Value>,
    history: Vec<Message>,
    streaming: bool,
) -> ProviderError {
    let request = rig::completion::CompletionRequest::new("Continue")
        .messages(history)
        .tools(vec![rig::completion::ToolDefinition {
            name: rig::message::ToolName::new("read").unwrap(),
            description: "Read a file".into(),
            parameters: json!({"type":"object", "properties":{"path":{"type":"string"}}, "required":["path"]}),
        }])
        .max_tokens(256)
        .additional_params(params);
    if streaming {
        use futures::StreamExt;
        let mut stream = match model.stream(request) {
            Ok(stream) => stream,
            Err(error) => return error,
        };
        while let Some(item) = stream.next().await {
            if let Err(error) = item {
                return error;
            }
        }
        panic!("expected mock rejection")
    } else {
        model.call(request).await.err().expect("mock rejection")
    }
}

#[tokio::test]
async fn opencode_go_wire_routes_headers_effort_and_errors() {
    for streaming in [false, true] {
        for name in ["glm-5.3-flash", "gpt-5.6-luna", "qwen3.8-max", "minimax-m3"] {
            let (client, captured) = capture_request().await;
            let params = opencode_go_reasoning_params_for_model(name, true, Some("low"));
            let model = client.completion_model(name.into());
            let error = match model {
                AnyModel::OpenAI(OpenAiModel::OpenCodeGoCompletions(m, _)) => {
                    send_request(m, params.clone(), vec![], streaming).await
                }
                AnyModel::OpenAI(OpenAiModel::OpenCodeGoResponses(m, _)) => {
                    send_request(m, params.clone(), vec![], streaming).await
                }
                AnyModel::OpenCodeGoMessages(m, _) => {
                    send_request(m, params.clone(), vec![], streaming).await
                }
                _ => panic!("unexpected Go model"),
            };
            assert!(error.to_string().contains("mock rejection"), "{error}");
            let (headers, body) = captured.await.unwrap();
            let (endpoint, auth) = match opencode_go_api(name) {
                OpenCodeGoApi::Completions => {
                    ("chat/completions", "authorization: bearer test-key")
                }
                OpenCodeGoApi::Responses => ("responses", "authorization: bearer test-key"),
                OpenCodeGoApi::Messages => ("messages", "x-api-key: test-key"),
            };
            assert!(
                headers.starts_with(&format!("post /zen/go/v1/{endpoint} ")),
                "{headers}"
            );
            assert!(headers.contains(auth), "{headers}");
            assert!(headers.contains("x-test-header: preserved"));
            assert!(
                headers.contains("x-opencode-session: test-session\r\n"),
                "{headers}"
            );
            assert!(
                headers.contains(concat!("user-agent: zerostack/", env!("CARGO_PKG_VERSION"))),
                "{headers}"
            );
            assert_eq!(body["model"], name);
            assert_eq!(body["stream"].as_bool().unwrap_or(false), streaming);
            for (key, value) in params.unwrap().as_object().unwrap() {
                assert_eq!(&body[key], value, "{name}: {body}");
            }
        }
    }
}

#[cfg(feature = "multimodal")]
#[tokio::test]
async fn opencode_go_wire_keeps_images_inside_tool_results() {
    use base64::Engine as _;
    use rig::completion::message::{
        AssistantContent, CallId, ToolCall, ToolFunction, ToolResult, ToolResultContent,
        UserContent,
    };

    let png = crate::extras::image_validate::test_png();
    let encoded = base64::prelude::BASE64_STANDARD.encode(&png);
    for name in ["glm-5.3-flash", "gpt-6-luna", "claude-haiku-5-5"] {
        let (client, captured) = capture_request().await;
        let call = CallId::from_wire("call-image");
        let assistant = Message::Assistant(rig::message::AssistantMessage::new(vec![
            AssistantContent::ToolCall(ToolCall::new(
                call.clone(),
                ToolFunction::new(
                    rig::message::ToolName::new("read").unwrap(),
                    json!({"path": "image.png"}),
                ),
            )),
        ]));
        let UserContent::Image(image) = UserContent::image_base64(
            encoded.clone(),
            Some(rig::message::ImageMediaType::PNG),
            None,
        ) else {
            unreachable!()
        };
        let result = Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call,
                name: rig::message::ToolName::new("read").unwrap(),
                is_error: false,
                content: vec![
                    ToolResultContent::Text(rig::message::Text::new("Read image")),
                    ToolResultContent::Image(image),
                ],
            })],
        };
        let model = client.completion_model(name.into());
        let error = match model {
            AnyModel::OpenAI(OpenAiModel::OpenCodeGoCompletions(m, _))
            | AnyModel::OpenAI(OpenAiModel::OpenCodeGoResponses(m, _)) => {
                send_request(m, None, vec![assistant, result], false).await
            }
            AnyModel::OpenCodeGoMessages(m, _) => {
                send_request(m, None, vec![assistant, result], false).await
            }
            _ => panic!("unexpected Go model"),
        };
        assert!(error.to_string().contains("mock rejection"), "{error}");
        let (_, body) = captured.await.unwrap();
        let wire = body.to_string();
        assert!(wire.contains(&encoded), "{name}: {body}");
        match opencode_go_api(name) {
            OpenCodeGoApi::Completions => {
                let messages = body["messages"].as_array().unwrap();
                let tool = messages
                    .iter()
                    .find(|message| message["role"] == "tool")
                    .unwrap();
                assert!(tool["content"].as_str().unwrap().contains("Read image"));
                assert!(messages.iter().any(|message| {
                    message["role"] == "user"
                        && message["content"].as_array().is_some_and(|content| {
                            content.iter().any(|part| part["type"] == "image_url")
                        })
                }));
            }
            OpenCodeGoApi::Responses => {
                let output = body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["type"] == "function_call_output")
                    .unwrap();
                assert_eq!(output["output"][1]["type"], "input_image");
            }
            OpenCodeGoApi::Messages => {
                let result = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|message| message["content"].as_array().into_iter().flatten())
                    .find(|item| item["type"] == "tool_result")
                    .unwrap();
                assert_eq!(result["content"][1]["type"], "image");
            }
        }
    }
}

#[tokio::test]
async fn opencode_go_model_refresh_sends_session_and_client_identity() {
    let (client, captured) = capture_request().await;
    assert!(client.list_models().await.is_err());
    let (headers, _) = captured.await.unwrap();
    assert!(headers.starts_with("get /zen/go/v1/models "), "{headers}");
    assert!(headers.contains("authorization: bearer test-key"));
    assert!(headers.contains("x-opencode-session: test-session\r\n"));
    assert!(headers.contains(concat!("user-agent: zerostack/", env!("CARGO_PKG_VERSION"))));
}

#[cfg(feature = "subagents")]
#[tokio::test]
async fn opencode_go_subagents_honor_quick_model_effort_and_token_limit() {
    for name in ["qwen3.8-max", "glm-5.3-flash", "gpt-5.6-luna"] {
        let (client, captured) = capture_request().await;
        let cfg: Config = serde_json::from_value(json!({
            "max_tokens": 256,
            "reasoning-effort": "high",
            "quick_models": {"go": {
                "provider": "opencode-go", "model": name, "reasoning-effort": "low"
            }}
        }))
        .unwrap();
        let agent = crate::extras::subagents::builder::build_explore_agent(
            client.completion_model(name.into()),
            1,
            &cfg,
            None,
            #[cfg(feature = "archmd")]
            None,
        )
        .await;
        let error = match agent {
            AnyAgent::Anthropic(a) => a.prompt("Say OK").await.err(),
            AnyAgent::OpenAI(OpenAiAgent::Responses(a)) => a.prompt("Say OK").await.err(),
            AnyAgent::OpenAI(OpenAiAgent::OpenCodeGoCompletions(a)) => {
                a.prompt("Say OK").await.err()
            }
            _ => panic!("unexpected Go agent"),
        }
        .unwrap();
        assert!(error.to_string().contains("mock rejection"), "{error}");
        let (_, body) = captured.await.unwrap();
        let params = opencode_go_reasoning_params_for_model(name, true, Some("low")).unwrap();
        for (key, value) in params.as_object().unwrap() {
            assert_eq!(&body[key], value, "{name}: {body}");
        }
        if name.starts_with("qwen") {
            assert_eq!(body["max_tokens"], 256);
        }
    }
}

#[tokio::test]
async fn opencode_go_wire_replays_reasoning_after_session_roundtrip() {
    use crate::session::{MessageRole, ProviderReasoning, Session};
    use rig::completion::message::Reasoning;

    for streaming in [false, true] {
        let (client, captured) = capture_request().await;
        let mut session = Session::new("opencode-go", "deepseek-v4-flash", 128000);
        session.add_message(MessageRole::User, "inspect");
        session.add_partial_assistant_output(
            "",
            vec![ProviderReasoning::from_rig(&Reasoning::new("test reasoning")).unwrap()],
        );
        session.add_tool_call_structured("read", &json!({"path":"test.txt"}), "call_1", None);
        session.add_tool_result_structured("read", "test contents", "call_1", None);
        session.add_message(MessageRole::Assistant, "done");
        let stored = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&stored).unwrap();
        let history = crate::agent::runner::convert_history(&restored);
        let AnyModel::OpenAI(OpenAiModel::OpenCodeGoCompletions(model, _)) =
            client.completion_model(session.model.to_string())
        else {
            panic!("Go completions")
        };
        send_request(model, None, history, streaming).await;
        let (_, body) = captured.await.unwrap();
        assert_eq!(
            body["messages"][1]["reasoning_content"], "test reasoning",
            "{body}"
        );
        assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
        assert_eq!(body["messages"][3]["reasoning_content"], "");
        assert!(body["messages"][0].get("reasoning_content").is_none());
    }
}

#[test]
fn opencode_oauth_rewrites_all_request_protocols_without_leaking_static_credentials() {
    let mut auth = crate::auth::opencode::Credential {
        access: "oauth-access".into(),
        refresh: "oauth-refresh".into(),
        expires: u64::MAX,
        account_id: "account".into(),
        org_id: Some("org-scoped".into()),
    };
    let config = json!({"providers":{"opencode-go":{
        "settings":{"baseURL":"https://opencode.ai/console/inference/v1"},
        "headers":{"x-route":"provider", "x-remove":"old", "x-opencode-session":"wrong", "Authorization":"Bearer wrong"},
        "models":{"model":{"modelID":"upstream-model", "headers":{"x-route":"model", "x-remove":null}}}
    }}});
    for (old, new) in [
        (
            "/zen/go/v1/chat/completions",
            "/console/inference/v1/chat/completions",
        ),
        ("/zen/go/v1/responses", "/console/inference/v1/responses"),
        ("/zen/go/v1/messages", "/console/inference/v1/messages"),
    ] {
        let request = http::Request::builder()
            .uri(format!("https://opencode.ai{old}"))
            .header("authorization", "Bearer expired-key")
            .header("x-api-key", "expired-key")
            .header("x-org-id", "stale-org")
            .header("host", "old-host")
            .header("x-opencode-session", "session")
            .body(())
            .unwrap();
        let (mut parts, _) = request.into_parts();
        let body = OpenCodeGoHttpClient::apply_oauth(
            &auth,
            &config,
            &mut parts,
            serde_json::to_vec(&json!({"model":"model", "stream":true, "messages":[]}))
                .unwrap()
                .into(),
        )
        .unwrap();
        assert_eq!(parts.uri.path(), new);
        assert_eq!(parts.headers["authorization"], "Bearer oauth-access");
        assert_eq!(parts.headers["x-org-id"], "org-scoped");
        assert_eq!(parts.headers["x-opencode-session"], "session");
        assert_eq!(parts.headers["x-route"], "model");
        assert!(!parts.headers.contains_key("x-remove"));
        assert!(!parts.headers.contains_key("x-api-key"));
        assert!(!parts.headers.contains_key("host"));
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["model"], "upstream-model");
        assert_eq!(value["stream"], true);
        // Reusing a request without organization scope must clear the old scope.
        auth.org_id = None;
        let body = serde_json::to_vec(&json!({"model":"model"}))
            .unwrap()
            .into();
        OpenCodeGoHttpClient::apply_oauth(&auth, &config, &mut parts, body).unwrap();
        assert!(!parts.headers.contains_key("x-org-id"));
        auth.org_id = Some("org-scoped".into());
    }
}

#[test]
fn opencode_oauth_model_list_merges_sparse_go_overrides_with_live_catalog() {
    let base = || {
        parse_opencode_go_models(
            r#"{"data":[{"id":"deepseek-v4.1-flash"},{"id":"glm-5.3-flash"}]}"#,
        )
        .unwrap()
    };
    let config = json!({"providers":{
        "opencode":{"models":{"deepseek-v4.1-flash":{"disabled":true}}},
        "opencode-go":{"models":{}}
    }});
    let models = OpenCodeGoClient::apply_model_overlay(base(), &config).unwrap();
    assert_eq!(models.len(), 2);
    assert!(models.iter().any(|model| model.id == "deepseek-v4.1-flash"));
    assert_eq!(models[0].context_length, base()[0].context_length);
    let models = OpenCodeGoClient::apply_model_overlay(base(), &json!({"providers":{}})).unwrap();
    assert_eq!(models.len(), 2);
    let config = json!({"providers":{"opencode-go":{"models":{
        "glm-5.3-flash":{"disabled":true},
        "deepseek-v4.1-flash":{"name":"DeepSeek", "limit":{"context":12345}},
        "new-model":{"modelID":"upstream-new"}
    }}}});
    let models = OpenCodeGoClient::apply_model_overlay(base(), &config).unwrap();
    assert_eq!(models.len(), 2);
    assert!(!models.iter().any(|model| model.id == "glm-5.3-flash"));
    let deepseek = models
        .iter()
        .find(|model| model.id == "deepseek-v4.1-flash")
        .unwrap();
    assert_eq!(deepseek.display, "DeepSeek");
    assert_eq!(deepseek.context_length, Some(12345));
    assert!(models.iter().any(|model| model.id == "new-model"));
}

#[test]
fn opencode_oauth_sparse_overlay_keeps_go_url_and_console_org_header() {
    let auth = crate::auth::opencode::Credential {
        access: "access".into(),
        refresh: "refresh".into(),
        expires: u64::MAX,
        account_id: "account".into(),
        org_id: Some("workspace".into()),
    };
    let config = json!({"providers":{"opencode-go":{"headers":{"x-opencode-org-id":"workspace"},"models":{}}}});
    let (mut parts, _) = http::Request::builder()
        .uri("https://opencode.ai/zen/go/v1/chat/completions")
        .body(())
        .unwrap()
        .into_parts();
    let body = OpenCodeGoHttpClient::apply_oauth(
        &auth,
        &config,
        &mut parts,
        serde_json::to_vec(&json!({"model":"deepseek-v4.1-flash","stream":true,"messages":[]}))
            .unwrap()
            .into(),
    )
    .unwrap();
    assert_eq!(
        parts.uri.to_string(),
        "https://opencode.ai/zen/go/v1/chat/completions"
    );
    assert_eq!(parts.headers["x-opencode-org-id"], "workspace");
    assert_eq!(parts.headers["authorization"], "Bearer access");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["model"],
        "deepseek-v4.1-flash"
    );
}

#[tokio::test]
#[ignore = "uses stored OpenCode OAuth credentials and makes billed model requests"]
async fn live_opencode_go_oauth_sparse_config_smoke() {
    use futures::StreamExt;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let client = build_opencode_go_client_inner(
        "dynamic-opencode-auth",
        None,
        http,
        Some("zerostack-oauth-overlay-smoke"),
        true,
    )
    .unwrap();
    let models = client.list_models().await.expect("OAuth model listing");
    assert!(models.iter().any(|model| model.id == "deepseek-v4.1-flash"));
    let AnyModel::OpenAI(OpenAiModel::OpenCodeGoCompletions(model, _)) =
        client.completion_model("deepseek-v4.1-flash".into())
    else {
        panic!("Go completions")
    };
    let request = || rig::completion::CompletionRequest::new("Reply OK.").max_tokens(32);
    model
        .call(request())
        .await
        .expect("non-streaming OAuth inference");
    let mut stream = model.stream(request()).expect("OAuth stream");
    let mut count = 0;
    while let Some(item) = stream.next().await {
        item.expect("streaming OAuth inference");
        count += 1;
    }
    assert!(count > 0);
}
