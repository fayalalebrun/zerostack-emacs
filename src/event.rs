use compact_str::CompactString;

use crate::session::ProviderReasoning;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub reasoning_tokens: u64,
}

impl From<rig::completion::Usage> for TokenUsage {
    fn from(usage: rig::completion::Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
        }
    }
}

impl TokenUsage {
    pub fn billable_input_tokens(self) -> u64 {
        self.input_tokens
    }

    pub fn billable_output_tokens(self) -> u64 {
        self.output_tokens
    }
}

impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .saturating_add(other.cache_creation_input_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
    }
}

#[cfg(test)]
mod tests {
    use super::TokenUsage;

    #[test]
    fn billable_input_uses_provider_reported_input_tokens() {
        let usage = TokenUsage {
            input_tokens: 100,
            cached_input_tokens: 30,
            cache_creation_input_tokens: 20,
            ..Default::default()
        };

        assert_eq!(usage.billable_input_tokens(), 100);
    }

    #[test]
    fn billable_output_uses_provider_reported_output_tokens() {
        let usage = TokenUsage {
            output_tokens: 40,
            reasoning_tokens: 15,
            ..Default::default()
        };

        assert_eq!(usage.billable_output_tokens(), 40);
    }
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Token(CompactString),
    Reasoning(CompactString),
    ToolCall {
        name: CompactString,
        args: serde_json::Value,
    },
    ToolResult {
        name: CompactString,
        output: CompactString,
    },
    SubagentToolCall {
        name: CompactString,
        args: serde_json::Value,
    },
    Error(CompactString),
    /// Provider call finished mid-stream. Carries the real provider-reported
    /// token usage for that call (when available). Used to update the
    /// status-bar estimate and to drive mid-turn compaction decisions
    /// independently of the local `len()/4` heuristic.
    CompletionCall {
        call_index: usize,
        usage: TokenUsage,
    },
    Done {
        response: CompactString,
        /// Cumulative usage across provider calls in this turn. Use for cost
        /// and lifetime token totals.
        usage: TokenUsage,
        /// Usage from the latest provider call in this turn. Use for current
        /// context pressure; summing prompt tokens across tool calls double-counts
        /// the same conversation prefix.
        context_usage: TokenUsage,
        reasoning: Vec<ProviderReasoning>,
    },
}

/// Events emitted by an isolated `/btw` side-question run. Kept as a separate
/// type from [`AgentEvent`] so that a side-question result can never be routed
/// through `handle_agent_event` (which mutates the session): the type system
/// enforces that `/btw` leaves no trace in conversation history.
#[derive(Debug, Clone)]
pub enum BtwEvent {
    Done {
        id: u32,
        response: CompactString,
        usage: TokenUsage,
    },
    Error {
        id: u32,
        message: CompactString,
    },
}

#[derive(Debug, Clone)]
pub enum UserEvent {
    Key(crossterm::event::KeyEvent),
    ScrollUp,
    ScrollDown,
    Resize,
    Paste(String),
    #[allow(dead_code)]
    MouseDown {
        row: u16,
        col: u16,
    },
    #[allow(dead_code)]
    MouseDrag {
        row: u16,
        col: u16,
    },
    #[allow(dead_code)]
    MouseUp {
        row: u16,
        col: u16,
    },
    /// An interactive MCP OAuth login finished in a background task. `error` is
    /// `None` on success. Handled by the TUI loop to reconnect the server.
    #[cfg(feature = "mcp")]
    McpLoginDone {
        server: CompactString,
        error: Option<CompactString>,
    },
}
