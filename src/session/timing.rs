use std::collections::HashMap;

use super::{MessageRole, ProviderCallPurpose, Session};

#[derive(Debug, PartialEq, Eq)]
pub struct ResponseTiming {
    pub turn: usize,
    pub block: usize,
    pub label: String,
    pub duration_ms: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CommandTiming {
    pub label: String,
    pub duration_ms: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct TimingReport {
    pub command_total_ms: u64,
    pub response_total_ms: u64,
    pub commands: Vec<CommandTiming>,
    pub responses: Vec<ResponseTiming>,
}

pub fn build_report(session: &Session) -> TimingReport {
    let mut turn_by_message = HashMap::new();
    let mut labels = HashMap::new();
    let mut turn = 0;
    for (index, message) in session.messages.iter().enumerate() {
        if message.role == MessageRole::User {
            turn += 1;
            turn_by_message.insert(index, turn);
            labels.insert(turn, shorten(&message.content));
        }
    }

    let mut responses: Vec<_> = session
        .provider_calls
        .iter()
        .filter(|call| call.purpose == ProviderCallPurpose::Agent)
        .filter_map(|call| {
            let turn = *turn_by_message.get(&call.message_index)?;
            Some(ResponseTiming {
                turn,
                block: call.call_index + 1,
                label: labels.get(&turn).cloned().unwrap_or_default(),
                duration_ms: call.duration_ms,
            })
        })
        .collect();
    responses.sort_by(|a, b| {
        b.duration_ms
            .cmp(&a.duration_ms)
            .then_with(|| a.turn.cmp(&b.turn))
            .then_with(|| a.block.cmp(&b.block))
    });

    let calls: HashMap<_, _> = session
        .messages
        .iter()
        .filter_map(|message| message.tool_call.as_ref())
        .map(|call| (call.id.as_str(), command_label(&call.name, &call.arguments)))
        .collect();
    let mut commands: Vec<_> = session
        .messages
        .iter()
        .filter_map(|message| message.tool_result.as_ref())
        .map(|result| CommandTiming {
            label: calls
                .get(result.id.as_str())
                .cloned()
                .unwrap_or_else(|| result.name.to_string()),
            duration_ms: result.duration_ms,
        })
        .collect();
    commands.sort_by(|a, b| b.duration_ms.cmp(&a.duration_ms));
    TimingReport {
        command_total_ms: commands.iter().map(|entry| entry.duration_ms).sum(),
        response_total_ms: responses.iter().map(|entry| entry.duration_ms).sum(),
        commands,
        responses,
    }
}

pub fn format_report(session: &Session) -> String {
    let report = build_report(session);
    let mut lines = vec!["session timing".to_string()];
    lines.push(format!(
        "  commands: {} total",
        format_duration(report.command_total_ms)
    ));
    if report.commands.is_empty() {
        lines.push("    no timing data".to_string());
    } else {
        lines.extend(report.commands.iter().map(|command| {
            format!(
                "    {}  {}",
                format_duration(command.duration_ms),
                command.label
            )
        }));
    }
    format_section(
        &mut lines,
        "text generation",
        report.response_total_ms,
        &report.responses,
    );
    lines.join("\n")
}

fn format_section(lines: &mut Vec<String>, name: &str, total_ms: u64, blocks: &[ResponseTiming]) {
    lines.push(format!("  {name}: {} total", format_duration(total_ms)));
    if blocks.is_empty() {
        lines.push("    no timing data".to_string());
    } else {
        lines.extend(blocks.iter().map(|timing| {
            format!(
                "    {}  turn {}, block {} — {}",
                format_duration(timing.duration_ms),
                timing.turn,
                timing.block,
                timing.label
            )
        }));
    }
}

fn command_label(name: &str, arguments: &serde_json::Value) -> String {
    let detail = match name {
        "bash" => arguments.get("command"),
        "read" | "write" | "edit" => arguments.get("path"),
        "grep" | "find_files" => arguments.get("pattern"),
        "list_dir" => arguments.get("path"),
        _ => None,
    }
    .and_then(serde_json::Value::as_str)
    .map(shorten);
    detail.map_or_else(|| name.to_string(), |detail| format!("{name}: {detail}"))
}

fn shorten(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let label: String = normalized.chars().take(60).collect();
    if normalized.chars().count() > 60 {
        format!("{label}…")
    } else if label.is_empty() {
        "(empty prompt)".to_string()
    } else {
        label
    }
}

fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::TokenUsage;

    #[test]
    fn sorts_individual_commands_and_response_turns() {
        let mut session = Session::new("openai", "model", 100_000);
        session.add_message(MessageRole::User, "first prompt");
        session.add_provider_call(0, TokenUsage::default(), 500);
        session.add_tool_call_structured(
            "bash",
            &serde_json::json!({"command": "true"}),
            "1",
            None,
        );
        session.add_tool_result_structured_with_context("bash", "", "1", None, Vec::new(), 900);
        session.add_provider_call(1, TokenUsage::default(), 700);
        session.add_message(MessageRole::Assistant, "done");
        session.add_message(MessageRole::User, "second prompt");
        session.add_provider_call(0, TokenUsage::default(), 2_000);
        session.add_tool_call_structured("read", &serde_json::json!({"path": "a"}), "2", None);
        session.add_tool_result_structured_with_context("read", "", "2", None, Vec::new(), 100);

        let report = build_report(&session);

        assert_eq!(report.command_total_ms, 1_000);
        assert_eq!(report.response_total_ms, 3_200);
        assert_eq!(report.responses.len(), 3);
        assert_eq!(
            report
                .commands
                .iter()
                .map(|t| t.label.as_str())
                .collect::<Vec<_>>(),
            vec!["bash: true", "read: a"]
        );
        assert_eq!(
            report.responses.iter().map(|t| t.turn).collect::<Vec<_>>(),
            vec![2, 1, 1]
        );
        assert_eq!(report.responses[1].duration_ms, 700);
        assert_eq!(report.responses[1].block, 2);
        let formatted = format_report(&session);
        assert!(formatted.contains("900ms  bash: true"));
        assert!(!formatted.contains("900ms  turn"));
        assert!(formatted.contains("text generation: 3.2s total"));
        assert!(formatted.contains("700ms  turn 1, block 2 — first prompt"));
    }

    #[test]
    fn empty_session_has_no_timing_rows() {
        let report = format_report(&Session::new("openai", "model", 100_000));
        assert!(report.contains("commands: 0ms total\n    no timing data"));
        assert!(report.contains("text generation: 0ms total\n    no timing data"));
    }
}
