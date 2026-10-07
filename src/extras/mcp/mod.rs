pub mod client;
pub mod config;
pub mod oauth;
pub mod tool;

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use compact_str::CompactString;
use tokio::time::Instant;
use tool::McpTool;

use crate::permission::ask::AskSender;
use crate::permission::checker::PermCheck;

const PENDING_REPORT_INTERVAL: Duration = Duration::from_secs(30);
static NEXT_OPERATION_ID: AtomicU64 = AtomicU64::new(1);

async fn trace_operation<T>(
    operation: &'static str,
    server: &str,
    tool: Option<&str>,
    future: impl Future<Output = T>,
) -> T {
    let id = NEXT_OPERATION_ID.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    tracing::debug!(target: "zerostack::mcp", id, operation, server, tool, "MCP operation started");
    let result = wait_with_pending_reports(future, PENDING_REPORT_INTERVAL, |elapsed| {
        tracing::debug!(target: "zerostack::mcp", id, operation, server, tool, elapsed_ms = elapsed.as_millis(), "MCP operation still pending");
    })
    .await;
    tracing::debug!(target: "zerostack::mcp", id, operation, server, tool, elapsed_ms = started.elapsed().as_millis(), "MCP operation finished");
    result
}

pub(crate) async fn timed_operation<T>(
    operation: &'static str,
    server: &str,
    tool: Option<&str>,
    deadline: Option<Duration>,
    future: impl Future<Output = T>,
) -> anyhow::Result<T> {
    let traced = trace_operation(operation, server, tool, future);
    let Some(deadline) = deadline else {
        return Ok(traced.await);
    };
    match tokio::time::timeout(deadline, traced).await {
        Ok(result) => Ok(result),
        Err(_) => {
            tracing::debug!(target: "zerostack::mcp", operation, server, tool, timeout_secs = deadline.as_secs(), "MCP operation timed out");
            anyhow::bail!(
                "MCP {operation} timed out after {} seconds for server '{server}'{}",
                deadline.as_secs(),
                tool.map(|name| format!(" and tool '{name}'"))
                    .unwrap_or_default()
            )
        }
    }
}

async fn wait_with_pending_reports<T>(
    future: impl Future<Output = T>,
    interval: Duration,
    mut report: impl FnMut(Duration),
) -> T {
    let started = Instant::now();
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = tokio::time::sleep(interval) => report(started.elapsed()),
        }
    }
}

pub struct McpClientManager {
    pub handles: Vec<client::McpClientHandle>,
    /// Connection failures collected during `connect_all`, to be surfaced by the
    /// TUI via the renderer. We do NOT log these at `warn` because that writes to
    /// stderr, which corrupts the alt-screen TUI (overlapping the input box).
    pub notices: Vec<CompactString>,
}

impl McpClientManager {
    pub async fn connect_all(configs: &HashMap<String, config::McpServerConfig>) -> Self {
        let mut handles = Vec::new();
        let mut notices = Vec::new();
        for (name, cfg) in configs {
            match timed_operation(
                "connect",
                name,
                None,
                cfg.connect_timeout(),
                client::McpClientHandle::connect(CompactString::new(name.clone()), cfg),
            )
            .await
            .and_then(|result| result)
            {
                Ok(handle) => {
                    tracing::info!("Connected to MCP server '{}'", name);
                    handles.push(handle);
                }
                Err(e) => {
                    tracing::debug!("Failed to connect to MCP server '{}': {e}", name);
                    notices.push(CompactString::new(format!(
                        "MCP server '{name}' not connected: {e}"
                    )));
                }
            }
        }
        Self { handles, notices }
    }

    /// Drain and return any pending connection notices.
    pub fn take_notices(&mut self) -> Vec<CompactString> {
        std::mem::take(&mut self.notices)
    }

    pub async fn collect_tools(
        &self,
        permission: Option<PermCheck>,
        ask_tx: Option<AskSender>,
    ) -> Vec<McpTool> {
        let mut all_tools = Vec::new();
        for handle in &self.handles {
            let peer = handle.peer();
            let server_name = handle.server_name.clone();
            match handle.list_tools().await {
                Ok(tools) => {
                    for definition in tools {
                        all_tools.push(McpTool {
                            server_name: server_name.clone(),
                            definition,
                            peer: peer.clone(),
                            permission: permission.clone(),
                            ask_tx: ask_tx.clone(),
                            timeout: handle.tool_timeout,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to list tools from MCP server '{}': {e}",
                        server_name
                    );
                }
            }
        }
        all_tools
    }

    /// (Re)connect a single server, replacing any existing handle for it.
    /// Used after an interactive OAuth login so the server's tools become
    /// available without restarting the session.
    pub async fn reconnect(
        &mut self,
        name: &str,
        cfg: &config::McpServerConfig,
    ) -> anyhow::Result<()> {
        let handle = timed_operation(
            "reconnect",
            name,
            None,
            cfg.connect_timeout(),
            client::McpClientHandle::connect(CompactString::new(name), cfg),
        )
        .await??;
        self.handles.retain(|h| h.server_name != name);
        self.handles.push(handle);
        Ok(())
    }

    pub async fn shutdown(self) {
        for handle in self.handles {
            let name = handle.server_name.clone();
            drop(handle);
            tracing::debug!("Disconnected from MCP server '{}'", name);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use compact_str::CompactString;
    use rmcp::model::*;
    use rmcp::service::{RequestContext, serve_client};
    use rmcp::{RoleServer, ServerHandler, ServiceExt};
    use tokio::time::timeout;

    #[derive(Clone)]
    struct StallingServer {
        stall_list: bool,
        stall_call: bool,
    }

    impl ServerHandler for StallingServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
                .with_protocol_version(ProtocolVersion::LATEST)
                .with_server_info(Implementation::new("stalling-server", "0.1.0"))
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            if self.stall_list {
                return future::pending().await;
            }
            Ok(ListToolsResult::with_all_items(vec![Tool::new(
                "hang",
                "test tool",
                Arc::new(serde_json::Map::new()),
            )]))
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResult, ErrorData> {
            if self.stall_call {
                return future::pending().await;
            }
            Ok(CallToolResult::success(vec![Content::text("done")]))
        }
    }

    async fn connected_server(
        server: StallingServer,
    ) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
        let (client_to_server, server_from_client) = tokio::io::duplex(8192);
        let (server_to_client, client_from_server) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let service = server
                .serve((server_from_client, server_to_client))
                .await
                .unwrap();
            let _ = service.waiting().await;
        });
        serve_client((), (client_from_server, client_to_server))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn handshake_can_remain_pending_without_an_upstream_error() {
        let (client_to_server, server_from_client) = tokio::io::duplex(8192);
        let (server_to_client, client_from_server) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let _streams = (server_from_client, server_to_client);
            future::pending::<()>().await;
        });

        assert!(
            timeout(
                Duration::from_millis(50),
                serve_client((), (client_from_server, client_to_server)),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn handshake_timeout_returns_an_error() {
        let (client_to_server, server_from_client) = tokio::io::duplex(8192);
        let (server_to_client, client_from_server) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let _streams = (server_from_client, server_to_client);
            future::pending::<()>().await;
        });

        let error = super::timed_operation(
            "connect",
            "test-server",
            None,
            Some(Duration::from_millis(10)),
            serve_client((), (client_from_server, client_to_server)),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("connect timed out"));
    }

    #[tokio::test]
    async fn tool_discovery_can_remain_pending_without_an_upstream_error() {
        let client = connected_server(StallingServer {
            stall_list: true,
            stall_call: false,
        })
        .await;

        assert!(
            timeout(Duration::from_millis(50), client.peer().list_all_tools())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn tool_call_can_remain_pending_without_an_upstream_error() {
        let client = connected_server(StallingServer {
            stall_list: false,
            stall_call: true,
        })
        .await;
        let tools = client.peer().list_all_tools().await.unwrap();
        assert_eq!(tools.len(), 1);

        assert!(
            timeout(
                Duration::from_millis(50),
                client.peer().call_tool(CallToolRequestParams::new("hang")),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn client_handle_applies_discovery_timeout() {
        let running_service = connected_server(StallingServer {
            stall_list: true,
            stall_call: false,
        })
        .await;
        let handle = super::client::McpClientHandle {
            server_name: CompactString::new("test-server"),
            running_service,
            discovery_timeout: Some(Duration::from_millis(10)),
            tool_timeout: None,
        };

        let error = handle.list_tools().await.unwrap_err();
        assert!(error.to_string().contains("list_tools timed out"));
    }

    #[tokio::test]
    async fn mcp_tool_applies_tool_timeout() {
        let client = connected_server(StallingServer {
            stall_list: false,
            stall_call: true,
        })
        .await;
        let definition = client.peer().list_all_tools().await.unwrap().remove(0);
        let tool = super::tool::McpTool {
            server_name: CompactString::new("test-server"),
            definition,
            peer: client.peer().clone(),
            permission: None,
            ask_tx: None,
            timeout: Some(Duration::from_millis(10)),
        };

        let error = tool
            .into_dynamic()
            .unwrap()
            .execute(serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("call_tool timed out"));
    }

    #[tokio::test]
    async fn timed_operation_returns_an_error_instead_of_hanging() {
        let error = super::timed_operation(
            "call_tool",
            "test-server",
            Some("hang"),
            Some(Duration::from_millis(10)),
            future::pending::<()>(),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("call_tool timed out"));
        assert!(error.to_string().contains("test-server"));
        assert!(error.to_string().contains("hang"));
    }

    #[tokio::test]
    async fn disabled_timeout_keeps_operation_unbounded() {
        assert!(
            timeout(
                Duration::from_millis(20),
                super::timed_operation(
                    "call_tool",
                    "test-server",
                    Some("hang"),
                    None,
                    future::pending::<()>(),
                ),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn completed_operation_is_not_reported_pending() {
        let reports = Arc::new(AtomicUsize::new(0));
        let report_count = reports.clone();
        let result =
            super::wait_with_pending_reports(async { 42 }, Duration::from_secs(1), move |_| {
                report_count.fetch_add(1, Ordering::Relaxed);
            })
            .await;

        assert_eq!(result, 42);
        assert_eq!(reports.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn pending_reports_do_not_cancel_operation() {
        let reports = Arc::new(AtomicUsize::new(0));
        let report_count = reports.clone();
        let result = super::wait_with_pending_reports(
            async {
                tokio::time::sleep(Duration::from_millis(35)).await;
                42
            },
            Duration::from_millis(5),
            move |_| {
                report_count.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await;

        assert_eq!(result, 42);
        assert!(reports.load(Ordering::Relaxed) >= 2);
    }
}
