use compact_str::CompactString;
use crossterm::style::Color;
use tokio::sync::mpsc;

use crate::agent::tools::todo::TODO_LIST;
use crate::cli::Cli;
use crate::config::{Config, ResolvedShowToolDetails};
use crate::context::ContextFiles;
use crate::event::AgentEvent;
#[cfg(feature = "mcp")]
use crate::extras::mcp::McpClientManager;
use crate::extras::status_signals::StatusSignals;
use crate::permission::ask::AskSender;
use crate::permission::checker::PermCheck;
use crate::provider::{AnyAgent, AnyClient};
use crate::sandbox::Sandbox;
use crate::session::storage::save_session;
use crate::session::{MessageRole, Session};
use crate::ui::events::sanitize_output;
use crate::ui::renderer::Renderer;
use crate::ui::slash::handle_compress;

use super::{C_AGENT, C_ERROR, C_TOOL, apply_current_prompt_mode};

#[cfg(feature = "mcp")]
#[allow(clippy::too_many_arguments)]
pub async fn ensure_agent(
    agent: &mut Option<AnyAgent>,
    client: &AnyClient,
    session: &mut Session,
    cli: &Cli,
    cfg: &Config,
    context: &ContextFiles,
    permission: &Option<PermCheck>,
    ask_tx: &Option<AskSender>,
    sandbox: &Sandbox,
    reasoning_enabled: bool,
    mcp_manager: Option<&McpClientManager>,
) {
    if agent.is_some() {
        return;
    }
    let model = client.completion_model(session.model.to_string());
    let temperature = crate::config::resolve_temperature(cli, cfg, &session.model);
    let extra_body = crate::config::resolve_extra_body(cfg, &session.model);
    *agent = Some(
        crate::provider::build_agent(
            model,
            cli,
            cfg,
            context,
            permission.clone(),
            ask_tx.clone(),
            sandbox.clone(),
            reasoning_enabled,
            crate::config::resolve_reasoning_effort(cli, cfg, &session.provider, &session.model)
                .as_deref(),
            temperature,
            extra_body,
            mcp_manager,
        )
        .await,
    );
    // Keep the pre-calibration context estimate in sync with the preamble we
    // just built (system prompt + tools + context files).
    session.overhead_tokens = crate::agent::builder::estimate_overhead(context, reasoning_enabled);
}

#[cfg(not(feature = "mcp"))]
#[allow(clippy::too_many_arguments)]
pub async fn ensure_agent(
    agent: &mut Option<AnyAgent>,
    client: &AnyClient,
    session: &mut Session,
    cli: &Cli,
    cfg: &Config,
    context: &ContextFiles,
    permission: &Option<PermCheck>,
    ask_tx: &Option<AskSender>,
    sandbox: &Sandbox,
    reasoning_enabled: bool,
) {
    if agent.is_some() {
        return;
    }
    let model = client.completion_model(session.model.to_string());
    let temperature = crate::config::resolve_temperature(cli, cfg, &session.model);
    let extra_body = crate::config::resolve_extra_body(cfg, &session.model);
    *agent = Some(
        crate::provider::build_agent(
            model,
            cli,
            cfg,
            context,
            permission.clone(),
            ask_tx.clone(),
            sandbox.clone(),
            reasoning_enabled,
            crate::config::resolve_reasoning_effort(cli, cfg, &session.provider, &session.model)
                .as_deref(),
            temperature,
            extra_body,
        )
        .await,
    );
    // Keep the pre-calibration context estimate in sync with the preamble we
    // just built (system prompt + tools + context files).
    session.overhead_tokens = crate::agent::builder::estimate_overhead(context, reasoning_enabled);
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_agent_event(
    event: AgentEvent,
    renderer: &mut Renderer,
    session: &mut Session,
    cfg: &Config,
    cli: &Cli,
    context: &mut ContextFiles,
    is_running: &mut bool,
    agent_rx: &mut Option<mpsc::Receiver<AgentEvent>>,
    agent_line_started: &mut bool,
    response_buf: &mut String,
    response_start_line: &mut Option<usize>,
    was_reasoning: &mut bool,
    show_reasoning: bool,
    agent: &mut Option<AnyAgent>,
    client: &mut AnyClient,
    loop_label: &mut Option<String>,
    permission: &Option<PermCheck>,
    ask_tx: &Option<AskSender>,
    sandbox: &Sandbox,
    status_signals: &Option<StatusSignals>,
    #[cfg(feature = "loop")] loop_state: &mut Option<crate::extras::r#loop::LoopState>,
    #[cfg(feature = "git-worktree")] wt_return_path: &mut Option<(String, String, String, bool)>,
    #[cfg(feature = "mcp")] mcp_manager: Option<&crate::extras::mcp::McpClientManager>,
) -> anyhow::Result<()> {
    match event {
        AgentEvent::Reasoning(text) => {
            if !show_reasoning {
                return Ok(());
            }
            if !*agent_line_started {
                renderer.write("< ", Color::DarkMagenta)?;
                *agent_line_started = true;
            }
            let safe = sanitize_output(&text);
            renderer.write(&safe, Color::DarkMagenta)?;
            *was_reasoning = true;
        }
        AgentEvent::Token(text) => {
            if *was_reasoning {
                renderer.write_line("", Color::White)?;
                *agent_line_started = false;
                *was_reasoning = false;
                response_buf.clear();
                *response_start_line = None;
            }
            let safe = sanitize_output(&text);
            response_buf.push_str(&safe);

            if response_buf.is_empty() {
                return Ok(());
            }

            let max_width = renderer.line_width();
            let mut styled = crate::ui::markdown::markdown_to_styled(response_buf, max_width);

            if !styled.is_empty() {
                styled[0].text = CompactString::from(format!("< {}", styled[0].text));
            }

            if let Some(start) = *response_start_line {
                renderer.replace_from(start, styled);
            } else {
                let start = renderer.buffer_len();
                *response_start_line = Some(start);
                renderer.replace_from(start, styled);
            }
            renderer.render_viewport()?;
            *agent_line_started = true;
        }
        AgentEvent::ToolCall {
            id,
            call_id,
            name,
            args,
        } => {
            *was_reasoning = false;
            if *agent_line_started {
                renderer.write_line("", Color::White)?;
                *agent_line_started = false;
            }
            response_buf.clear();
            *response_start_line = None;
            session.add_tool_call_structured(&name, &args, &id, call_id.as_deref());
            save_session_if_enabled(session, cli, renderer)?;
            let line = format!(
                "◈ {}",
                crate::ui::utils::format_tool_call_summary(&name, &args)
            );
            renderer.write_line(&sanitize_output(&line), C_TOOL)?;
        }
        AgentEvent::SubagentToolCall { name, args } => {
            session.add_subagent_tool_call(&name, &args);
            save_session_if_enabled(session, cli, renderer)?;
            let line = format!(
                "⌥ {}",
                crate::ui::utils::format_tool_call_summary(&name, &args)
            );
            renderer.write_line(&sanitize_output(&line), C_TOOL)?;
        }
        AgentEvent::ToolResult {
            id,
            call_id,
            name,
            output,
        } => {
            session.add_tool_result_structured(&name, &output, &id, call_id.as_deref());
            save_session_if_enabled(session, cli, renderer)?;
            if name == "todo_write" {
                let list = TODO_LIST.lock().unwrap_or_else(|e| e.into_inner());
                if list.is_empty() {
                    renderer.write_line("tasks cleared", Color::DarkGrey)?;
                } else {
                    let total = list.len();
                    let completed = list.iter().filter(|t| t.status == "completed").count();
                    renderer.write_line(
                        &format!("tasks  {} done / {} total", completed, total),
                        C_TOOL,
                    )?;
                    for item in list.iter() {
                        let icon = match item.status.as_str() {
                            "completed" => "[x]",
                            "in_progress" => "[>]",
                            "cancelled" => "[-]",
                            _ => "[ ]",
                        };
                        let status_color = match item.status.as_str() {
                            "completed" => Color::Green,
                            "in_progress" => C_TOOL,
                            "cancelled" => Color::DarkGrey,
                            _ => Color::DarkGrey,
                        };
                        let priority_mark = match item.priority.as_str() {
                            "high" => "!!",
                            "medium" => "! ",
                            _ => "  ",
                        };
                        renderer.write_line(
                            &format!("  {} {} {}", icon, priority_mark, item.content),
                            status_color,
                        )?;
                    }
                }
            } else {
                let show_details = cfg
                    .show_tool_details
                    .as_ref()
                    .map(|s| s.resolve())
                    .unwrap_or(ResolvedShowToolDetails::Limited(3));
                match show_details {
                    ResolvedShowToolDetails::Off => {}
                    ResolvedShowToolDetails::Limited(max_lines) => {
                        let sanitized = sanitize_output(&output);
                        let char_count = sanitized.chars().count();
                        let lines: Vec<&str> = sanitized.lines().collect();
                        if lines.len() > max_lines {
                            let shown = lines[..max_lines].join("\n");
                            let summary = format!(
                                "◈ result ({} chars, {} lines, showing {}):\n{}",
                                char_count,
                                lines.len(),
                                max_lines,
                                shown
                            );
                            renderer.write_line(&summary, Color::DarkGrey)?;
                        } else {
                            let summary =
                                format!("◈ result ({} chars):\n{}", char_count, sanitized);
                            renderer.write_line(&summary, Color::DarkGrey)?;
                        }
                    }
                    ResolvedShowToolDetails::Unlimited => {
                        let sanitized = sanitize_output(&output);
                        let char_count = sanitized.chars().count();
                        let summary = format!("◈ result ({} chars):\n{}", char_count, sanitized);
                        renderer.write_line(&summary, Color::DarkGrey)?;
                    }
                }
            }
        }
        AgentEvent::Done {
            response,
            usage,
            context_usage,
            reasoning,
        } => {
            handle_agent_done(
                response,
                usage,
                context_usage,
                reasoning,
                renderer,
                session,
                cfg,
                cli,
                context,
                is_running,
                agent_rx,
                agent_line_started,
                response_buf,
                response_start_line,
                was_reasoning,
                agent,
                client,
                loop_label,
                permission,
                ask_tx,
                sandbox,
                status_signals,
                #[cfg(feature = "loop")]
                loop_state,
                #[cfg(feature = "git-worktree")]
                wt_return_path,
                #[cfg(feature = "mcp")]
                mcp_manager,
            )
            .await?;
        }
        AgentEvent::CompletionCall { call_index: _, usage } => {
            let real = usage.context_tokens();
            if real > session.total_estimated_tokens {
                session.total_estimated_tokens = real;
            }
        }
        AgentEvent::Error(e) => {
            *was_reasoning = false;
            let safe = sanitize_output(&e);
            renderer.write_line(&format!("error: {}", safe), C_ERROR)?;
            *is_running = false;
            if let Some(ss) = status_signals.as_ref() {
                ss.send_stop();
            }
            *agent_rx = None;
            *agent_line_started = false;
            response_buf.clear();
            *response_start_line = None;
            save_session_if_enabled(session, cli, renderer)?;
        }
    }
    Ok(())
}

fn save_session_if_enabled(
    session: &Session,
    cli: &Cli,
    renderer: &mut Renderer,
) -> anyhow::Result<()> {
    if !cli.no_session
        && let Err(e) = save_session(session)
    {
        renderer.write_line(&format!("warning: failed to save session: {}", e), C_ERROR)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_agent_done(
    response: CompactString,
    usage: crate::event::TokenUsage,
    context_usage: crate::event::TokenUsage,
    reasoning: Vec<crate::session::ProviderReasoning>,
    renderer: &mut Renderer,
    session: &mut Session,
    cfg: &Config,
    cli: &Cli,
    context: &mut ContextFiles,
    is_running: &mut bool,
    agent_rx: &mut Option<mpsc::Receiver<AgentEvent>>,
    agent_line_started: &mut bool,
    response_buf: &mut String,
    response_start_line: &mut Option<usize>,
    was_reasoning: &mut bool,
    agent: &mut Option<AnyAgent>,
    client: &mut AnyClient,
    loop_label: &mut Option<String>,
    permission: &Option<PermCheck>,
    ask_tx: &Option<AskSender>,
    sandbox: &Sandbox,
    status_signals: &Option<StatusSignals>,
    #[cfg(feature = "loop")] loop_state: &mut Option<crate::extras::r#loop::LoopState>,
    #[cfg(feature = "git-worktree")] wt_return_path: &mut Option<(String, String, String, bool)>,
    #[cfg(feature = "mcp")] mcp_manager: Option<&crate::extras::mcp::McpClientManager>,
) -> anyhow::Result<()> {
    *was_reasoning = false;

    if !response_buf.is_empty() {
        let max_width = renderer.line_width();
        let mut styled = crate::ui::markdown::markdown_to_styled(response_buf, max_width);
        if !styled.is_empty() {
            styled[0].text = CompactString::from(format!("< {}", styled[0].text));
        }
        if let Some(start) = *response_start_line {
            renderer.replace_from(start, styled);
            renderer.render_viewport()?;
        }
    } else if !*agent_line_started {
        renderer.write("< ", C_AGENT)?;
    }

    renderer.write_line("", Color::White)?;
    renderer.write_line("", Color::White)?;
    session.add_message_with_reasoning_and_usage(
        MessageRole::Assistant,
        &response,
        reasoning,
        Some(context_usage.into()),
    );
    let billable_input_tokens = usage.billable_input_tokens();
    let billable_output_tokens = usage.billable_output_tokens();
    session.total_input_tokens = session
        .total_input_tokens
        .saturating_add(billable_input_tokens);
    session.total_cached_input_tokens = session
        .total_cached_input_tokens
        .saturating_add(usage.cached_input_tokens);
    session.total_output_tokens = session
        .total_output_tokens
        .saturating_add(billable_output_tokens);

    session.total_cost += crate::pricing::estimate_cost(
        billable_input_tokens,
        billable_output_tokens,
        session.input_token_cost,
        session.output_token_cost,
    );
    // Kept for old saved-session compatibility; current context pressure is
    // derived from the latest assistant message's provider_usage.

    session.set_calibration(usage.input_tokens, usage.output_tokens);
    *agent_line_started = false;
    response_buf.clear();
    *response_start_line = None;

    #[cfg(feature = "loop")]
    let loop_running = loop_state.as_ref().is_some_and(|ls| ls.active);
    #[cfg(not(feature = "loop"))]
    let loop_running = false;

    let qm = crate::config::quick_models_map(cfg);

    #[cfg(feature = "memory")]
    let reserve = crate::extras::memory::effective_reserve(
        cfg.resolve_reserve_tokens(&session.model, &qm),
        context.memory.as_deref(),
    );
    #[cfg(not(feature = "memory"))]
    let reserve = cfg.resolve_reserve_tokens(&session.model, &qm);

    if !loop_running
        && cfg.resolve_compact_enabled()
        && session.needs_compaction(reserve)
        && !cli.no_session
    {
        renderer.write_line("auto-compacting...", Color::DarkGrey)?;
        let compress_result = handle_compress(
            None,
            agent,
            client,
            renderer,
            session,
            cli,
            cfg,
            context,
            true,
            permission,
            ask_tx,
            sandbox,
            #[cfg(feature = "mcp")]
            mcp_manager,
        )
        .await;
        if let Err(e) = compress_result {
            renderer.write_line(&format!("auto-compact error: {}", e), C_ERROR)?;
        }
    }

    if !cli.no_session
        && let Err(e) = save_session(session)
    {
        renderer.write_line(&format!("warning: failed to save session: {}", e), C_ERROR)?;
    }
    *is_running = false;
    if let Some(ss) = status_signals.as_ref() {
        ss.send_stop();
    }
    *agent_rx = None;

    #[cfg(feature = "loop")]
    if let Some(ls) = loop_state
        && ls.active
    {
        let summary: String = response
            .chars()
            .take(crate::extras::r#loop::SUMMARY_TRUNCATION_CHARS)
            .collect();
        ls.last_summary = Some(summary.clone());

        let validation_output = if let Some(cmd) = &ls.run_cmd {
            let shell = if cfg!(windows) { "powershell" } else { "sh" };
            let shell_arg = if cfg!(windows) { "-Command" } else { "-c" };
            match tokio::process::Command::new(shell)
                .arg(shell_arg)
                .arg(cmd)
                .output()
                .await
            {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let combined = if stderr.is_empty() {
                        stdout
                    } else {
                        format!("{}\n{}", stdout, stderr)
                    };
                    Some(combined)
                }
                Err(e) => {
                    let msg = format!("error: {}", e);
                    Some(msg)
                }
            }
        } else {
            None
        };
        ls.last_run_output = validation_output.clone();

        let _ = crate::extras::r#loop::transcript::save_iteration(
            &session.id,
            ls.iteration,
            &ls.build_prompt(),
            &response,
            validation_output.as_deref(),
            &summary,
        );

        ls.iteration += 1;

        if ls.should_stop() {
            renderer.write_line(
                &format!(
                    "[loop] max iterations ({}) reached, stopping",
                    ls.iteration - 1
                ),
                C_AGENT,
            )?;
            ls.active = false;
            *loop_label = None;
        } else {
            let prompt = ls.build_prompt();
            *agent = Some({
                let model = client.completion_model(session.model.to_string());
                let temperature = crate::config::resolve_temperature(cli, cfg, &session.model);
                let extra_body = crate::config::resolve_extra_body(cfg, &session.model);
                crate::provider::build_agent(
                    model,
                    cli,
                    cfg,
                    context,
                    permission.clone(),
                    ask_tx.clone(),
                    sandbox.clone(),
                    true,
                    crate::config::resolve_reasoning_effort(
                        cli,
                        cfg,
                        &session.provider,
                        &session.model,
                    )
                    .as_deref(),
                    temperature,
                    extra_body,
                    #[cfg(feature = "mcp")]
                    mcp_manager,
                )
                .await
            });
            let runner = agent
                .as_ref()
                .unwrap()
                .clone()
                .spawn_runner(prompt, Vec::new());
            *agent_rx = Some(runner.event_rx);
            *is_running = true;
            if let Some(ss) = status_signals.as_ref() {
                ss.send_start();
            }
            *loop_label = Some(ls.iteration_label());
            renderer.write_line(
                &format!("[loop] launching {}", ls.iteration_label()),
                C_AGENT,
            )?;
        }
    }

    #[cfg(feature = "git-worktree")]
    if let Some((main_path, wt_path, branch, force)) = wt_return_path.take() {
        crate::extras::git_worktree::cleanup_worktree(&wt_path, &branch, &main_path, force);
        match std::env::set_current_dir(&main_path) {
            Ok(()) => {
                session.working_dir = compact_str::CompactString::new(&main_path);
                context.reload();
                apply_current_prompt_mode(context, permission);
                *agent = Some({
                    let model = client.completion_model(session.model.to_string());
                    let temperature = crate::config::resolve_temperature(cli, cfg, &session.model);
                    let extra_body = crate::config::resolve_extra_body(cfg, &session.model);
                    crate::provider::build_agent(
                        model,
                        cli,
                        cfg,
                        context,
                        permission.clone(),
                        ask_tx.clone(),
                        sandbox.clone(),
                        true,
                        crate::config::resolve_reasoning_effort(
                            cli,
                            cfg,
                            &session.provider,
                            &session.model,
                        )
                        .as_deref(),
                        temperature,
                        extra_body,
                        #[cfg(feature = "mcp")]
                        mcp_manager,
                    )
                    .await
                });
                crate::ui::events::render_session(renderer, session, cli, cfg, context)?;
                renderer.write_line(
                    &format!("merged and returned to main repo at {}", main_path),
                    C_AGENT,
                )?;
            }
            Err(e) => {
                renderer.write_line(
                    &format!("warning: failed to change back to main repo: {}", e),
                    C_ERROR,
                )?;
            }
        }
    }

    Ok(())
}
