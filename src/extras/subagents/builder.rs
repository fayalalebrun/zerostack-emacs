use crate::agent::tools;
use crate::extras::subagents::prompt;
use crate::provider::{AnyAgent, AnyModel, OpenAiAgent, OpenAiModel};
use rig::agent::{Agent, AgentBuilder};

#[allow(clippy::too_many_arguments)]
fn build_explore_agent_inner<M: Into<rig::DynModel<rig::operation::Completion>>>(
    model: M,
    max_turns: usize,
    max_tokens: Option<u64>,
    max_text_file_size: u64,
    max_read_lines: u64,
    max_grep_results: u64,
    max_find_results: u64,
    max_list_dir_entries: Option<u64>,
    // OpenRouter `provider.order` pin for `anthropic/*` (see `AnyClient::completion_model`).
    additional_params: Option<serde_json::Value>,
    agents: Option<&str>,
    #[cfg(feature = "archmd")] architecture: Option<&str>,
) -> Agent {
    let mut preamble = prompt::explore_prompt();

    if let Some(agents) = agents
        && !agents.is_empty()
    {
        preamble.push_str("\n\n");
        preamble.push_str(agents);
    }

    #[cfg(feature = "archmd")]
    if let Some(arch) = architecture
        && !arch.is_empty()
    {
        preamble.push_str("\n\n");
        preamble.push_str(arch);
    }

    if let Some(s) = crate::session::storage::load_suffix() {
        preamble.push_str("\n\n---\n\n");
        preamble.push_str(&s);
    }

    let context_tracker = tools::new_context_tracker(std::iter::empty());
    let mut builder = AgentBuilder::new(model)
        .preamble(&preamble)
        .default_max_turns(max_turns)
        .tool(
            tools::ReadTool::new(None, None, Some(max_text_file_size), max_read_lines)
                .with_context_tracker(context_tracker.clone()),
        )
        .tool(
            tools::GrepTool::new(None, None, max_grep_results)
                .with_context_tracker(context_tracker.clone()),
        )
        .tool(
            tools::FindFilesTool::new(None, None, max_find_results)
                .with_context_tracker(context_tracker.clone()),
        )
        .tool(
            tools::ListDirTool::new(None, None, max_list_dir_entries)
                .with_context_tracker(context_tracker.clone()),
        );

    #[cfg(feature = "memory")]
    {
        builder = builder.tool(crate::extras::memory::MemoryRead::new(None, None));
    }

    #[cfg(feature = "memory")]
    {
        builder = builder.tool(crate::extras::memory::MemorySearch::new(None, None));
    }

    if let Some(max_tokens) = max_tokens {
        builder = builder.max_tokens(max_tokens);
    }

    if let Some(params) = additional_params {
        builder = builder.additional_params(params);
    }

    builder.build()
}

pub(crate) async fn build_explore_agent(
    model: AnyModel,
    max_turns: usize,
    cfg: &crate::config::Config,
    agents: Option<String>,
    #[cfg(feature = "archmd")] architecture: Option<String>,
) -> AnyAgent {
    let max_text_file_size = cfg.max_text_file_size.unwrap_or(10 * 1024 * 1024);
    let max_read_lines = cfg.resolve_subagent_max_read_lines();
    let max_grep_results = cfg.resolve_subagent_max_grep_results();
    let max_find_results = cfg.resolve_subagent_max_find_results();
    let max_list_dir_entries = cfg.resolve_subagent_max_list_dir_entries();
    let agents_ref = agents.as_deref();
    #[cfg(feature = "archmd")]
    let arch_ref = architecture.as_deref();
    let go_reasoning_params = |model: &str| {
        let effort = crate::config::resolve_reasoning_effort(
            &crate::cli::Cli::default(),
            cfg,
            "opencode-go",
            model,
        );
        crate::provider::opencode_go_reasoning_params_for_model(model, true, effort.as_deref())
    };
    match model {
        AnyModel::OpenRouter(m, extra) => AnyAgent::OpenRouter(build_explore_agent_inner(
            m,
            max_turns,
            None,
            max_text_file_size,
            max_read_lines,
            max_grep_results,
            max_find_results,
            max_list_dir_entries,
            extra,
            agents_ref,
            #[cfg(feature = "archmd")]
            arch_ref,
        )),
        AnyModel::OpenAI(m) => AnyAgent::OpenAI(match m {
            OpenAiModel::Responses(m) => OpenAiAgent::Responses(build_explore_agent_inner(
                m,
                max_turns,
                None,
                max_text_file_size,
                max_read_lines,
                max_grep_results,
                max_find_results,
                max_list_dir_entries,
                None,
                agents_ref,
                #[cfg(feature = "archmd")]
                arch_ref,
            )),
            OpenAiModel::Completions(m) => OpenAiAgent::Completions(build_explore_agent_inner(
                m,
                max_turns,
                None,
                max_text_file_size,
                max_read_lines,
                max_grep_results,
                max_find_results,
                max_list_dir_entries,
                None,
                agents_ref,
                #[cfg(feature = "archmd")]
                arch_ref,
            )),
            OpenAiModel::Codex(m) => OpenAiAgent::Codex(build_explore_agent_inner(
                m,
                max_turns,
                None,
                max_text_file_size,
                max_read_lines,
                max_grep_results,
                max_find_results,
                max_list_dir_entries,
                None,
                agents_ref,
                #[cfg(feature = "archmd")]
                arch_ref,
            )),
            OpenAiModel::OpenCodeGoResponses(m, model) => {
                OpenAiAgent::Responses(build_explore_agent_inner(
                    m,
                    max_turns,
                    None,
                    max_text_file_size,
                    max_read_lines,
                    max_grep_results,
                    max_find_results,
                    max_list_dir_entries,
                    go_reasoning_params(&model),
                    agents_ref,
                    #[cfg(feature = "archmd")]
                    arch_ref,
                ))
            }
            OpenAiModel::OpenCodeGoCompletions(m, model) => {
                OpenAiAgent::OpenCodeGoCompletions(build_explore_agent_inner(
                    m,
                    max_turns,
                    None,
                    max_text_file_size,
                    max_read_lines,
                    max_grep_results,
                    max_find_results,
                    max_list_dir_entries,
                    go_reasoning_params(&model),
                    agents_ref,
                    #[cfg(feature = "archmd")]
                    arch_ref,
                ))
            }
        }),
        AnyModel::OpenCodeGoMessages(m, model) => {
            let agent = build_explore_agent_inner(
                m,
                max_turns,
                Some(crate::cli::Cli::default().resolve_max_tokens(cfg)),
                max_text_file_size,
                max_read_lines,
                max_grep_results,
                max_find_results,
                max_list_dir_entries,
                go_reasoning_params(&model),
                agents_ref,
                #[cfg(feature = "archmd")]
                arch_ref,
            );
            AnyAgent::Anthropic(agent)
        }
        AnyModel::Anthropic(m) => AnyAgent::Anthropic(build_explore_agent_inner(
            m,
            max_turns,
            None,
            max_text_file_size,
            max_read_lines,
            max_grep_results,
            max_find_results,
            max_list_dir_entries,
            None,
            agents_ref,
            #[cfg(feature = "archmd")]
            arch_ref,
        )),
        AnyModel::Gemini(m) => AnyAgent::Gemini(build_explore_agent_inner(
            m,
            max_turns,
            None,
            max_text_file_size,
            max_read_lines,
            max_grep_results,
            max_find_results,
            max_list_dir_entries,
            None,
            agents_ref,
            #[cfg(feature = "archmd")]
            arch_ref,
        )),
        AnyModel::Ollama(m) => AnyAgent::Ollama(build_explore_agent_inner(
            m,
            max_turns,
            None,
            max_text_file_size,
            max_read_lines,
            max_grep_results,
            max_find_results,
            max_list_dir_entries,
            None,
            agents_ref,
            #[cfg(feature = "archmd")]
            arch_ref,
        )),
        #[cfg(test)]
        AnyModel::Test(c) => AnyAgent::Test(crate::provider::TestAgent {
            prompts: c.prompts,
            sandbox: crate::sandbox::Sandbox::new(false, "bwrap"),
        }),
    }
}
