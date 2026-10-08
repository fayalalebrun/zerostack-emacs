use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use rig::agent::{AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, RequestPatch};
use rig::completion::ToolDefinition;
use rig::message::{Message, ToolResultContent, UserContent};
use rig::tool::{DynamicTool, ToolExecutionError, ToolOutput};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::tool::McpTool;

const SEARCH_NAME: &str = "mcp_search_tools";

pub(crate) fn qualified_name(server: &str, tool: &str) -> String {
    let digest = Sha256::digest(format!("{}:{server}{tool}", server.len()));
    let prefix: String = format!("mcp_{server}_{tool}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(47)
        .collect();
    format!(
        "{prefix}_{:x}",
        &digest[..8]
            .iter()
            .fold(0u64, |n, b| (n << 8) | u64::from(*b))
    )
}

#[derive(Clone, Default)]
pub(crate) struct DiscoveryHook {
    pub eager: Arc<Mutex<Vec<String>>>,
    deferred: Arc<HashSet<String>>,
}

impl DiscoveryHook {
    fn active(&self, history: &[Message], prompt: &Message) -> Vec<String> {
        let mut active = self.eager.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for message in history.iter().chain(std::iter::once(prompt)) {
            if let Message::User { content } = message {
                for item in content {
                    if let UserContent::ToolResult(result) = item
                        && result.name.as_str() == SEARCH_NAME
                        && !result.is_error
                    {
                        for content in &result.content {
                            if let ToolResultContent::Text(text) = content
                                && let Ok(names) = serde_json::from_str::<SearchResult>(&text.text)
                            {
                                active.extend(
                                    names
                                        .tools
                                        .into_iter()
                                        .map(|tool| tool.name)
                                        .filter(|name| self.deferred.contains(name)),
                                );
                            }
                        }
                    }
                }
            }
        }
        active.sort();
        active.dedup();
        active
    }
}

impl AgentHook for DiscoveryHook {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        CompletionCallAction::Patch(
            RequestPatch::new().active_tools(self.active(event.history, event.prompt)),
        )
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    server: Option<String>,
}

#[derive(Clone, serde::Serialize, Deserialize)]
struct SearchEntry {
    name: String,
    server: String,
    tool: String,
    description: String,
}

#[derive(serde::Serialize, Deserialize)]
struct SearchResult {
    tools: Vec<SearchEntry>,
}

fn search(entries: &[SearchEntry], args: SearchArgs) -> Result<SearchResult, ToolExecutionError> {
    let terms: Vec<_> = args
        .query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    if terms.is_empty() {
        return Err(ToolExecutionError::other(
            "query must contain a search term",
        ));
    }
    if let Some(server) = &args.server
        && !entries.iter().any(|entry| &entry.server == server)
    {
        return Err(ToolExecutionError::other(format!(
            "Unknown MCP server: {server}"
        )));
    }
    let mut matches: Vec<_> = entries
        .iter()
        .filter(|entry| {
            args.server
                .as_ref()
                .is_none_or(|server| server == &entry.server)
        })
        .filter_map(|entry| {
            let name = format!("{} {}", entry.server, entry.tool).to_lowercase();
            let description = entry.description.to_lowercase();
            let score: usize = terms
                .iter()
                .map(|term| {
                    usize::from(name.contains(term)) * 4 + usize::from(description.contains(term))
                })
                .sum();
            (score > 0).then_some((score, entry))
        })
        .collect();
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    Ok(SearchResult {
        tools: matches
            .into_iter()
            .take(5)
            .map(|(_, entry)| {
                let mut entry = entry.clone();
                entry.description = entry.description.chars().take(240).collect();
                entry
            })
            .collect(),
    })
}

pub(crate) fn deferred_tools(tools: Vec<McpTool>) -> (Vec<DynamicTool>, DiscoveryHook) {
    let mut entries = Vec::new();
    let mut dynamic = Vec::new();
    for tool in tools {
        let name = qualified_name(&tool.server_name, &tool.definition.name);
        let entry = SearchEntry {
            name: name.clone(),
            server: tool.server_name.to_string(),
            tool: tool.definition.name.to_string(),
            description: tool
                .definition
                .description
                .as_deref()
                .unwrap_or("")
                .to_string(),
        };
        match tool.into_dynamic_named(&name) {
            Ok(tool) => {
                dynamic.push(tool);
                entries.push(entry);
            }
            Err(error) => tracing::warn!(%error, "invalid MCP tool name"),
        }
    }
    let hook = DiscoveryHook {
        deferred: Arc::new(entries.iter().map(|entry| entry.name.clone()).collect()),
        ..Default::default()
    };
    let mut servers: Vec<_> = entries.iter().map(|entry| entry.server.clone()).collect();
    servers.sort();
    servers.dedup();
    let summary = servers
        .iter()
        .map(|server| {
            format!(
                "{server} ({} tools)",
                entries
                    .iter()
                    .filter(|entry| &entry.server == server)
                    .count()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    dynamic.push(DynamicTool::new(
        rig::message::ToolName::new(SEARCH_NAME).unwrap(),
        format!("Discover external tools by action or exact tool name. Available MCP servers: {summary}. Returns up to five matches; their full input schemas become available as directly callable named tools on the next model turn. Search again if tools disappear after compaction. Do not guess tool names or arguments."),
        serde_json::json!({"type":"object","properties":{"query":{"type":"string","description":"Action, keywords, or exact tool name"},"server":{"type":"string","description":"Optional exact MCP server name"}},"required":["query"],"additionalProperties":false}),
        move |args| {
            let result = serde_json::from_value(args).map_err(|error| ToolExecutionError::other(format!("Invalid search arguments: {error}"))).and_then(|args| search(&entries, args)).and_then(|result| serde_json::to_string(&result).map_err(|error| ToolExecutionError::other(error.to_string())));
            Box::pin(async move { result.map(ToolOutput::text) })
        },
    ));
    (dynamic, hook)
}

pub(crate) fn eager_names(definitions: &[ToolDefinition], hook: &DiscoveryHook) {
    *hook.eager.lock().unwrap_or_else(|e| e.into_inner()) = definitions
        .iter()
        .map(|definition| definition.name.to_string())
        .filter(|name| !hook.deferred.contains(name))
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::message::{CallId, Text, ToolName, ToolResult};

    fn entry(server: &str, tool: &str, description: &str) -> SearchEntry {
        SearchEntry {
            name: qualified_name(server, tool),
            server: server.into(),
            tool: tool.into(),
            description: description.into(),
        }
    }

    fn discovery_message(entries: Vec<SearchEntry>, failed: bool) -> Message {
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: CallId::from_wire("search-1"),
                name: ToolName::new(SEARCH_NAME).unwrap(),
                is_error: failed,
                content: vec![ToolResultContent::Text(Text::new(
                    serde_json::to_string(&SearchResult { tools: entries }).unwrap(),
                ))],
            })],
        }
    }

    #[test]
    fn names_are_stable_bounded_and_server_qualified() {
        let name = qualified_name("notion", "search");
        assert_eq!(name, qualified_name("notion", "search"));
        assert_ne!(name, qualified_name("slack", "search"));
        assert_ne!(
            qualified_name("a-b", "search"),
            qualified_name("a_b", "search")
        );
        let name = qualified_name(&"😀".repeat(100), &"工具".repeat(100));
        assert!(name.len() <= 64);
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }

    #[test]
    fn search_ranks_names_caps_results_and_validates_inputs() {
        let mut entries: Vec<_> = (0..10)
            .map(|i| entry("notion", &format!("misc_{i}"), "search pages"))
            .collect();
        entries.push(entry("slack", "search", "Find messages"));
        let found = search(
            &entries,
            SearchArgs {
                query: "search".into(),
                server: None,
            },
        )
        .unwrap();
        assert_eq!(found.tools.len(), 5);
        assert_eq!(found.tools[0].server, "slack");
        let filtered = search(
            &entries,
            SearchArgs {
                query: "search".into(),
                server: Some("notion".into()),
            },
        )
        .unwrap();
        assert!(filtered.tools.iter().all(|entry| entry.server == "notion"));
        assert!(
            search(
                &entries,
                SearchArgs {
                    query: "---".into(),
                    server: None
                }
            )
            .is_err()
        );
        assert!(
            search(
                &entries,
                SearchArgs {
                    query: "search".into(),
                    server: Some("missing".into())
                }
            )
            .is_err()
        );
        assert!(
            search(
                &entries,
                SearchArgs {
                    query: "unmatched".into(),
                    server: None
                }
            )
            .unwrap()
            .tools
            .is_empty()
        );
        let large = vec![entry("notion", "search", &"😀".repeat(1000))];
        assert_eq!(
            search(
                &large,
                SearchArgs {
                    query: "search".into(),
                    server: None
                }
            )
            .unwrap()
            .tools[0]
                .description
                .chars()
                .count(),
            240
        );
    }

    #[test]
    fn discovery_is_history_scoped_and_survives_session_roundtrip() {
        let entry = entry("linear", "list_issues", "Find issues");
        let hook = DiscoveryHook {
            deferred: Arc::new(HashSet::from([entry.name.clone()])),
            ..Default::default()
        };
        *hook.eager.lock().unwrap() = vec!["read".into(), SEARCH_NAME.into()];
        let prompt = Message::user("find issues");
        assert!(!hook.active(&[], &prompt).contains(&entry.name));
        let found = discovery_message(vec![entry.clone()], false);
        assert!(hook.active(&[found.clone()], &prompt).contains(&entry.name));
        assert!(hook.active(&[], &found).contains(&entry.name));
        assert!(
            !hook
                .active(&[discovery_message(vec![entry.clone()], true)], &prompt)
                .contains(&entry.name)
        );
        let unknown = self::entry("unknown", "missing", "not registered");
        assert!(
            !hook
                .active(&[discovery_message(vec![unknown.clone()], false)], &prompt)
                .contains(&unknown.name)
        );
        let mut session = crate::session::Session::new("openai", "test", 1000);
        session.add_message(crate::session::MessageRole::User, "find issues");
        session.add_tool_call_structured(
            SEARCH_NAME,
            &serde_json::json!({"query":"issues"}),
            "search-1",
            None,
        );
        session.add_tool_result_structured(
            SEARCH_NAME,
            &serde_json::to_string(&SearchResult {
                tools: vec![entry.clone()],
            })
            .unwrap(),
            "search-1",
            None,
        );
        let restored = serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
        assert!(
            hook.active(&crate::agent::runner::convert_history(&restored), &prompt)
                .contains(&entry.name)
        );
        session.compress("summary".into(), 3, 10);
        assert!(
            !hook
                .active(&crate::agent::runner::convert_history(&session), &prompt)
                .contains(&entry.name)
        );
    }
    #[tokio::test]
    async fn wire_defers_schemas_then_calls_discovered_tool_directly() {
        use futures::StreamExt;
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        let entries = vec![
            entry("linear", "list_issues", "Find issues"),
            entry("notion", "search", "Find pages"),
        ];
        let selected = entries[0].name.clone();
        let hidden = entries[1].name.clone();
        let hook = DiscoveryHook {
            deferred: Arc::new(entries.iter().map(|entry| entry.name.clone()).collect()),
            ..Default::default()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let expected_name = selected.clone();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for turn in 0..3 {
                let (socket, _) = listener.accept().await.unwrap();
                let mut reader = tokio::io::BufReader::new(socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).await.unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.unwrap();
                requests.push(serde_json::from_slice::<serde_json::Value>(&body).unwrap());
                let delta = match turn {
                    0 => {
                        serde_json::json!({"tool_calls":[{"index":0,"id":"search-1","type":"function","function":{"name":SEARCH_NAME,"arguments":"{\"query\":\"issues\"}"}}]})
                    }
                    1 => {
                        serde_json::json!({"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":expected_name,"arguments":"{\"query\":\"test\"}"}}]})
                    }
                    _ => serde_json::json!({"content":"done"}),
                };
                let chunk = serde_json::json!({"id":"test","object":"chat.completion.chunk","created":0,"model":"test","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                let end = serde_json::json!({"id":"test","object":"chat.completion.chunk","created":0,"model":"test","choices":[{"index":0,"delta":{},"finish_reason":if turn < 2 {"tool_calls"} else {"stop"}}]});
                let body = format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n");
                reader.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            requests
        });
        let client = rig::providers::openai::OpenAIConfig::new("test")
            .with_base_url(base)
            .connect(rig::http_client::ReqwestClient::from(
                reqwest::Client::builder().no_proxy().build().unwrap(),
            ));
        let search_tool = DynamicTool::new(
            rig::message::ToolName::new(SEARCH_NAME).unwrap(),
            "Find tools",
            serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}),
            move |args| {
                let result = search(&entries, serde_json::from_value(args).unwrap()).unwrap();
                Box::pin(
                    async move { Ok(ToolOutput::text(serde_json::to_string(&result).unwrap())) },
                )
            },
        );
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let tool = DynamicTool::new(
            rig::message::ToolName::new(&selected).unwrap(),
            "Find issues",
            serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}),
            move |_| {
                count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Box::pin(async { Ok(ToolOutput::text("found issues")) })
            },
        );
        let hidden_tool = DynamicTool::new(
            rig::message::ToolName::new(&hidden).unwrap(),
            "SECRET_SCHEMA_SENTINEL",
            serde_json::json!({"type":"object","properties":{"unused":{"type":"string","description":"SECRET_SCHEMA_SENTINEL"}}}),
            |_| Box::pin(async { panic!("undiscovered tool executed") }),
        );
        let agent = rig::agent::AgentBuilder::new(client.chat("test"))
            .default_max_turns(3)
            .dynamic_tools(vec![search_tool, tool, hidden_tool])
            .add_hook(hook.clone())
            .build();
        eager_names(&agent.tool_server_handle().static_tool_defs(), &hook);
        let mut stream = agent.prompt("find issues").stream();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(item) = stream.next().await {
                item.unwrap();
            }
        })
        .await
        .unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests[0]["tools"].as_array().unwrap().len(), 1);
        assert_eq!(requests[0]["tools"][0]["function"]["name"], SEARCH_NAME);
        assert!(!requests[0].to_string().contains(&selected));
        for request in &requests[1..] {
            assert_eq!(request["tools"].as_array().unwrap().len(), 2);
            assert!(
                request["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["function"]["name"] == selected)
            );
            assert!(!request.to_string().contains("SECRET_SCHEMA_SENTINEL"));
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
