use super::*;
use rig::completion::{CompletionError, Message};
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
                for headers in [
                    client.completions.headers(),
                    client.responses.headers(),
                    client.messages.headers(),
                ] {
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
        let id = client.responses.headers()["x-opencode-session"]
            .to_str()
            .unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok());
        assert!(ids.insert(id.to_string()));
        let cloned = client.clone();
        for headers in [
            cloned.responses.headers(),
            cloned.completions.headers(),
            cloned.messages.headers(),
        ] {
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

async fn send_request<M: CompletionModel>(
    model: M,
    params: Option<Value>,
    history: Vec<Message>,
    streaming: bool,
) -> CompletionError {
    let request = model
        .completion_request("Continue")
        .messages(history)
        .max_tokens(256)
        .additional_params_opt(params)
        .build();
    if streaming {
        use futures::StreamExt;
        let mut stream = match model.stream(request).await {
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
        model
            .completion(request)
            .await
            .err()
            .expect("mock rejection")
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
    use rig::completion::Prompt;

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
        assert_eq!(body["messages"][1]["reasoning_content"], "test reasoning");
        assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
        assert_eq!(body["messages"][3]["reasoning_content"], "");
        assert!(body["messages"][0].get("reasoning_content").is_none());
    }
}
