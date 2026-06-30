use compact_str::CompactString;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

#[cfg(feature = "subagents")]
use crate::extras::subagents::{builder, clone_subagent_event_tx, try_with_config};

use crate::agent::tools::{AskSender, PermCheck, ToolError, check_perm};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct GoalState {
    pub content: String,
    pub status: CompactString,
    pub priority: CompactString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_status: Option<CompactString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_summary: Option<String>,
}

#[derive(Deserialize)]
pub struct GoalUpdateArgs {
    #[serde(default)]
    pub clear: bool,
    pub content: Option<String>,
    pub status: Option<CompactString>,
    pub priority: Option<CompactString>,
    pub evidence: Option<String>,
}

pub static GOAL: std::sync::Mutex<Option<GoalState>> = std::sync::Mutex::new(None);
static GOAL_NUDGE_COUNT: std::sync::Mutex<Option<(String, usize)>> = std::sync::Mutex::new(None);

#[cfg(feature = "subagents")]
const GOAL_EVALUATOR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

#[cfg(feature = "subagents")]
async fn evaluate_goal(goal: &GoalState, action: &str) -> Result<String, ToolError> {
    let (client, model_name, max_turns, config, agents) = try_with_config(|cfg| {
        (
            cfg.client.clone(),
            cfg.model_name.clone(),
            cfg.max_turns,
            cfg.config.clone(),
            cfg.agents.clone(),
        )
    })
    .ok_or_else(|| {
        ToolError::Msg(format!(
            "Cannot mark goal {action}: subagent evaluator is not initialized"
        ))
    })?;
    #[cfg(feature = "archmd")]
    let architecture = try_with_config(|cfg| cfg.architecture.clone()).unwrap_or(None);
    #[cfg(not(feature = "archmd"))]
    let architecture: Option<String> = None;

    let prompt = goal_evaluator_prompt(goal, action);
    let model = client.completion_model(model_name);
    let event_tx = clone_subagent_event_tx();
    let work = async move {
        let agent =
            builder::build_explore_agent(model, max_turns, &config, agents, architecture).await;
        agent
            .run_subagent(&prompt, max_turns, event_tx.as_ref())
            .await
            .map_err(|e| ToolError::Msg(e.to_string()))
    };
    tokio::time::timeout(GOAL_EVALUATOR_TIMEOUT, work)
        .await
        .map_err(|_| ToolError::Msg("goal evaluator timed out after 300s".to_string()))?
}

#[cfg(not(feature = "subagents"))]
async fn evaluate_goal(_goal: &GoalState, action: &str) -> Result<String, ToolError> {
    Err(ToolError::Msg(format!(
        "Cannot mark goal {action}: subagents feature is required for automatic evaluation"
    )))
}

fn goal_evaluator_prompt(goal: &GoalState, action: &str) -> String {
    let rubric = if action == "blocked" {
        "Check only whether the evidence proves this goal is blocked by an external dependency, missing user input, or permission denial. Unfinished work, uncertainty, remaining implementation effort, test failures, or low confidence are not blockers."
    } else {
        "Check only whether the evidence supports this one goal being completed."
    };
    format!(
        "You are a narrow independent goal evaluator.\n\nRequested status: {action}\n\nGoal:\n{}\n\nClaimed evidence:\n{}\n\n{rubric} Do not infer broad intent or trust claims without support. Use read/search tools only if needed. Return a concise report whose first non-empty line starts with exactly one of: PASS, FAIL, or INSUFFICIENT. Cite the evidence you relied on.",
        goal.content,
        goal.evidence.as_deref().unwrap_or("").trim()
    )
}

pub(crate) fn parse_evaluator_verdict(report: &str) -> &'static str {
    let first = report
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_ascii_uppercase();
    if first.starts_with("PASS") {
        "PASS"
    } else if first.starts_with("FAIL") {
        "FAIL"
    } else {
        "INSUFFICIENT"
    }
}

pub struct UpdateGoal {
    pub permission: Option<PermCheck>,
    pub ask_tx: Option<AskSender>,
}

impl UpdateGoal {
    pub fn new(permission: Option<PermCheck>, ask_tx: Option<AskSender>) -> Self {
        UpdateGoal { permission, ask_tx }
    }
}

impl Tool for UpdateGoal {
    const NAME: &'static str = "goal_update";

    type Error = ToolError;
    type Args = GoalUpdateArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "goal_update".to_string(),
            description: "Set, update, clear, or complete the single active implementation goal. Completing requires concrete evidence and automatically runs an independent evaluator.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "clear": { "type": "boolean", "description": "Clear the active goal" },
                    "content": { "type": "string", "description": "Goal description" },
                    "status": { "type": "string", "description": "pending, in_progress, completed, blocked, or cancelled" },
                    "priority": { "type": "string", "description": "high, medium, or low" },
                    "evidence": { "type": "string", "description": "Concrete evidence for completion: commands run, output observed, files changed, or user confirmation" }
                }
            }),
        }
    }

    async fn call(&self, args: GoalUpdateArgs) -> Result<String, ToolError> {
        let coaching = check_perm(&self.permission, &self.ask_tx, "goal_update", "").await?;

        if args.clear {
            clear_goal_state();
            return Ok(prefix_coaching(coaching, "Goal cleared.".to_string()));
        }

        let current = GOAL.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut goal = GoalState {
            content: args
                .content
                .or_else(|| current.as_ref().map(|g| g.content.clone()))
                .ok_or_else(|| {
                    ToolError::Msg("goal_update requires content for a new goal".to_string())
                })?,
            status: args
                .status
                .or_else(|| current.as_ref().map(|g| g.status.clone()))
                .unwrap_or_else(|| CompactString::new("in_progress")),
            priority: args
                .priority
                .or_else(|| current.as_ref().map(|g| g.priority.clone()))
                .unwrap_or_else(|| CompactString::new("medium")),
            evidence: args
                .evidence
                .or_else(|| current.as_ref().and_then(|g| g.evidence.clone())),
            evaluator_status: None,
            evaluator_summary: None,
        };

        if matches!(goal.status.as_str(), "blocked" | "cancelled") {
            let evidence = goal.evidence.as_deref().unwrap_or("").trim();
            if evidence.len() < 20 {
                return Err(ToolError::Msg(format!(
                    "Cannot mark '{}' {}: provide a concrete external reason in evidence. Blocked is only for external dependency, missing user input, or permission denial; cancelled is only for user-requested scope changes.",
                    goal.content, goal.status
                )));
            }
        }

        if goal.status == "blocked" {
            let report = evaluate_goal(&goal, "blocked").await?;
            let verdict = parse_evaluator_verdict(&report);
            goal.evaluator_status = Some(CompactString::new(verdict));
            goal.evaluator_summary = Some(report.trim().to_string());
            if verdict != "PASS" {
                return Err(ToolError::Msg(format!(
                    "Cannot mark '{}' blocked: evaluator returned {verdict}.\n\n{}",
                    goal.content,
                    report.trim()
                )));
            }
        }

        if goal.status == "completed" {
            let evidence = goal.evidence.as_deref().unwrap_or("").trim();
            if evidence.is_empty() {
                return Err(ToolError::Msg(format!(
                    "Cannot mark '{}' completed: provide concrete evidence first.",
                    goal.content
                )));
            }
            let report = evaluate_goal(&goal, "completed").await?;
            let verdict = parse_evaluator_verdict(&report);
            goal.evaluator_status = Some(CompactString::new(verdict));
            goal.evaluator_summary = Some(report.trim().to_string());
            if verdict != "PASS" {
                return Err(ToolError::Msg(format!(
                    "Cannot mark '{}' completed: evaluator returned {verdict}.\n\n{}",
                    goal.content,
                    report.trim()
                )));
            }
        }

        let output = format_goal(&goal);
        *GOAL.lock().unwrap_or_else(|e| e.into_inner()) = Some(goal);
        Ok(prefix_coaching(coaching, output))
    }
}

fn prefix_coaching(coaching: Option<String>, output: String) -> String {
    match coaching {
        Some(c) => format!("{}\n\n{}", c, output),
        None => output,
    }
}

pub(crate) fn current_goal_state() -> Option<GoalState> {
    GOAL.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(crate) fn set_goal_state(goal: Option<GoalState>) {
    *GOAL.lock().unwrap_or_else(|e| e.into_inner()) = goal;
    *GOAL_NUDGE_COUNT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub(crate) fn clear_goal_state() {
    set_goal_state(None);
}

pub(crate) fn next_goal_nudge(max_nudges: usize) -> Option<String> {
    let goal = GOAL.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
    if matches!(goal.status.as_str(), "completed" | "cancelled" | "blocked") {
        return None;
    }
    let mut guard = GOAL_NUDGE_COUNT.lock().unwrap_or_else(|e| e.into_inner());
    let count = match guard.as_mut() {
        Some((content, count)) if *content == goal.content => count,
        _ => {
            *guard = Some((goal.content.clone(), 0));
            &mut guard.as_mut().unwrap().1
        }
    };
    if *count >= max_nudges {
        return None;
    }
    *count += 1;
    Some(format!(
        "Open goal remains (nudge {}/{}): {}\n\nContinue working, or complete it with concrete evidence so goal_update can run the evaluator. Only mark blocked for an external dependency, missing user input, or permission denial, with concrete evidence. Only mark cancelled for a user-requested scope change, with evidence. Do not end the turn while this goal is still open.",
        *count, max_nudges, goal.content
    ))
}

pub(crate) fn format_goal(goal: &GoalState) -> String {
    let icon = match goal.status.as_str() {
        "completed" => "[x]",
        "in_progress" => "[>]",
        "cancelled" => "[-]",
        _ => "[ ]",
    };
    let mut result = format!("Goal:\n  {} [{}] {}\n", icon, goal.priority, goal.content);
    if let Some(evidence) = goal.evidence.as_deref().filter(|s| !s.trim().is_empty()) {
        result.push_str(&format!("      evidence: {}\n", evidence.trim()));
    }
    if let Some(status) = goal
        .evaluator_status
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        result.push_str(&format!("      evaluator: {}", status.trim()));
        if let Some(summary) = goal
            .evaluator_summary
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            result.push_str(&format!(" — {}", summary.trim()));
        }
        result.push('\n');
    }
    result
}
