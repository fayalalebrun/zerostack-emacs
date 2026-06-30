use crate::agent::tools::UpdateGoal;
use crate::agent::tools::goal::{
    GoalUpdateArgs, clear_goal_state, current_goal_state, next_goal_nudge, parse_evaluator_verdict,
};
use compact_str::CompactString;
use rig::tool::Tool;

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn reset_goal() {
    clear_goal_state();
}

fn args(content: &str, status: &str) -> GoalUpdateArgs {
    GoalUpdateArgs {
        clear: false,
        content: Some(content.to_string()),
        status: Some(CompactString::new(status)),
        priority: Some(CompactString::new("high")),
        evidence: None,
    }
}

#[tokio::test]
async fn definition_name() {
    let _guard = TEST_LOCK.lock().unwrap();
    let tool = UpdateGoal::new(None, None);
    let def = tool.definition(String::new()).await;
    assert_eq!(def.name, "goal_update");
}

#[tokio::test]
async fn clear_goal() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_goal();
    let tool = UpdateGoal::new(None, None);
    let result = tool
        .call(GoalUpdateArgs {
            clear: true,
            content: None,
            status: None,
            priority: None,
            evidence: None,
        })
        .await;
    assert!(result.is_ok());
    assert!(result.unwrap().contains("cleared"));
}

#[tokio::test]
async fn open_goal_nudges_until_limit() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_goal();
    let tool = UpdateGoal::new(None, None);
    tool.call(args("Ship feature", "in_progress"))
        .await
        .unwrap();

    assert!(next_goal_nudge(2).unwrap().contains("nudge 1/2"));
    assert!(next_goal_nudge(2).unwrap().contains("nudge 2/2"));
    assert!(next_goal_nudge(2).is_none());
    reset_goal();
}

#[tokio::test]
async fn blocked_goal_requires_concrete_external_reason() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_goal();
    let tool = UpdateGoal::new(None, None);
    let result = tool.call(args("Ship feature", "blocked")).await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("external reason"));
}

#[tokio::test]
async fn blocked_goal_with_reason_runs_evaluator() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_goal();
    let tool = UpdateGoal::new(None, None);
    let mut goal = args("Ship feature", "blocked");
    goal.evidence =
        Some("Permission denied for the required production deploy command.".to_string());

    let result = tool.call(goal).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("subagent evaluator is not initialized")
    );
}

#[tokio::test]
async fn cancelled_goal_does_not_nudge_when_evidence_is_provided() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_goal();
    let tool = UpdateGoal::new(None, None);
    let mut goal = args("Ship feature", "cancelled");
    goal.evidence = Some("User explicitly requested dropping this goal from scope.".to_string());
    tool.call(goal).await.unwrap();

    assert!(next_goal_nudge(2).is_none());
}

#[tokio::test]
async fn completed_goal_requires_evidence() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_goal();
    let tool = UpdateGoal::new(None, None);
    let result = tool.call(args("Ship feature", "completed")).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("provide concrete evidence")
    );
}

#[test]
fn parses_evaluator_verdict_from_first_non_empty_line() {
    assert_eq!(parse_evaluator_verdict("PASS\nverified"), "PASS");
    assert_eq!(parse_evaluator_verdict("\nfail: missing evidence"), "FAIL");
    assert_eq!(parse_evaluator_verdict("maybe"), "INSUFFICIENT");
}

#[tokio::test]
async fn completed_goal_runs_evaluator() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_goal();
    let tool = UpdateGoal::new(None, None);
    let mut goal = args("Ship feature", "completed");
    goal.evidence = Some("cargo test passed".to_string());

    let result = tool.call(goal).await;

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("subagent evaluator is not initialized")
    );
}

#[tokio::test]
async fn in_progress_goal_with_evidence_is_accepted_without_evaluator() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_goal();
    let tool = UpdateGoal::new(None, None);
    let mut goal = args("Ship feature", "in_progress");
    goal.evidence = Some("cargo test passed".to_string());
    let output = tool.call(goal).await.unwrap();

    assert!(output.contains("evidence: cargo test passed"));
    assert_eq!(current_goal_state().unwrap().content, "Ship feature");

    reset_goal();
}
