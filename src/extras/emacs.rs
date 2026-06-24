#[cfg(unix)]
mod imp {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use anyhow::Context as _;
    use compact_str::CompactString;
    use pulldown_cmark::{
        Alignment as MdAlignment, Event as MdEvent, Options as MdOptions, Parser as MdParser,
        Tag as MdTag, TagEnd as MdTagEnd,
    };
    use serde::{Deserialize, Serialize};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::process::Command as ProcessCommand;
    use tokio::sync::{Mutex, broadcast, mpsc};
    use tokio::time::{Duration, timeout};

    use crate::agent::runner::convert_history;
    use crate::agent::tools::bash::{BashLiveOutputRequest, set_bash_live_output_sender};
    use crate::cli::Cli;
    use crate::config::{self, Config};
    use crate::context::ContextFiles;
    use crate::event::AgentEvent;
    use crate::extras::status_signals::StatusSignals;
    use crate::permission::ask::{AskReceiver, AskRequest, AskSender, UserDecision};
    use crate::permission::checker::PermCheck;
    use crate::provider::{AnyClient, build_agent};
    use crate::sandbox::Sandbox;
    use crate::session::{MessageRole, PermissionAllowEntry, Session};
    use crate::ui::events::{format_time, sanitize_output};
    use crate::ui::markdown::word_wrap;
    use crate::ui::utils::{display_width, format_tool_call_summary, suggest_pattern};

    const PROTOCOL_VERSION: u32 = 1;
    const DEFAULT_COLS: usize = 100;
    const EVENT_BUFFER: usize = 512;
    const ARTIFACT_PREVIEW_CHARS: usize = 240;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SessionMeta {
        pub session_id: String,
        pub pid: u32,
        pub cwd: String,
        pub model: String,
        pub provider: String,
        pub created_at: String,
        pub updated_at: String,
        pub title: String,
        pub tokens: u64,
        pub context_window: u64,
        pub protocol: u32,
        pub socket: String,
        #[serde(default = "default_thinking_level")]
        pub thinking: String,
        #[serde(default)]
        pub reasoning_effort_supported: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub reasoning_effort: Option<String>,
    }

    fn default_thinking_level() -> String {
        "on".to_string()
    }

    struct Registration {
        dir: PathBuf,
        socket_path: PathBuf,
    }

    struct Server {
        client: Mutex<AnyClient>,
        cli: Cli,
        cfg: Config,
        context: Mutex<ContextFiles>,
        session: Mutex<Session>,
        permission: Option<PermCheck>,
        ask_tx: Option<AskSender>,
        sandbox: Sandbox,
        status_signals: Option<StatusSignals>,
        #[cfg(feature = "mcp")]
        mcp_manager: Mutex<Option<crate::extras::mcp::McpClientManager>>,
        events: broadcast::Sender<String>,
        mutable: Mutex<MutableState>,
        registry_dir: PathBuf,
        socket_path: PathBuf,
    }

    struct MutableState {
        seq: u64,
        cols: usize,
        line_count: usize,
        running: bool,
        reasoning_enabled: bool,
        reasoning_effort: Option<CompactString>,
        abort_handle: Option<tokio::task::AbortHandle>,
        turn: u64,
        #[cfg(feature = "loop")]
        loop_state: Option<crate::extras::r#loop::LoopState>,
        next_artifact_id: u64,
        next_permission_id: u64,
        pending_permissions: HashMap<u64, AskRequest>,
        last_event_at: Option<String>,
    }

    struct CompactionOutcome {
        compacted: bool,
        messages: usize,
        saved_tokens: u64,
        message: String,
    }

    struct AttachmentOutcome {
        kind: &'static str,
        path: PathBuf,
        bytes: u64,
        mime: Option<String>,
        message: String,
    }

    impl AttachmentOutcome {
        fn fields(&self) -> String {
            format!(
                " :kind {} :path {} :bytes {}{} :message {}",
                self.kind,
                sexp_quote(self.path.to_string_lossy().as_ref()),
                self.bytes,
                self.mime
                    .as_deref()
                    .map(|mime| format!(" :mime {}", sexp_quote(mime)))
                    .unwrap_or_default(),
                sexp_quote(&self.message),
            )
        }
    }

    #[derive(Debug, Clone)]
    struct AttachmentItem {
        index: usize,
        kind: &'static str,
        path: PathBuf,
        bytes: u64,
        mime: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ArtifactInfo {
        kind: &'static str,
        path: PathBuf,
        mime: &'static str,
        bytes: usize,
        preview: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct LatexInfo {
        id: String,
        source: String,
        display: bool,
        artifact: ArtifactInfo,
        svg_artifact: Option<ArtifactInfo>,
        line_start: usize,
        col_start: usize,
        line_end: usize,
        col_end: usize,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct WireSpan {
        text: String,
        face: &'static str,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct WireLine {
        text: String,
        face: &'static str,
        spans: Vec<WireSpan>,
        artifact: Option<ArtifactInfo>,
        latex: Vec<LatexInfo>,
        message_index: Option<usize>,
        role: Option<MessageRole>,
    }

    impl WireLine {
        fn new(text: impl Into<String>, face: &'static str) -> Self {
            Self {
                text: text.into(),
                face,
                spans: Vec::new(),
                artifact: None,
                latex: Vec::new(),
                message_index: None,
                role: None,
            }
        }

        fn with_artifact(
            text: impl Into<String>,
            face: &'static str,
            artifact: ArtifactInfo,
        ) -> Self {
            Self {
                text: text.into(),
                face,
                spans: Vec::new(),
                artifact: Some(artifact),
                latex: Vec::new(),
                message_index: None,
                role: None,
            }
        }

        fn with_spans(spans: Vec<WireSpan>, face: &'static str) -> Self {
            let text = spans.iter().map(|span| span.text.as_str()).collect();
            Self {
                text,
                face,
                spans,
                artifact: None,
                latex: Vec::new(),
                message_index: None,
                role: None,
            }
        }

        fn push_latex(&mut self, latex: LatexInfo) {
            self.latex.push(latex);
        }

        fn with_source(mut self, message_index: usize, role: MessageRole) -> Self {
            self.message_index = Some(message_index);
            self.role = Some(role);
            self
        }
    }

    fn role_atom(role: MessageRole) -> &'static str {
        match role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::ToolCall => "tool-call",
            MessageRole::ToolResult => "tool-result",
            MessageRole::SubagentToolCall => "subagent-tool-call",
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct LocatedLatexSpan {
        source: String,
        display: bool,
        line_start: usize,
        col_start: usize,
        line_end: usize,
        col_end: usize,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Sexp {
        Atom(String),
        Str(String),
        List(Vec<Sexp>),
    }

    #[derive(Debug)]
    struct Command {
        name: String,
        args: HashMap<String, Sexp>,
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn serve(
        client: AnyClient,
        cli: Cli,
        cfg: Config,
        context: ContextFiles,
        session: Session,
        permission: Option<PermCheck>,
        ask_tx: Option<AskSender>,
        ask_rx: Option<AskReceiver>,
        sandbox: Sandbox,
        status_signals: Option<StatusSignals>,
    ) -> anyhow::Result<()> {
        let (registration, listener) = Registration::create(&session)?;
        let initial_line_count =
            render_session_lines(&session, &cli, &cfg, &context, DEFAULT_COLS).len();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let server = Arc::new(Server {
            client: Mutex::new(client),
            cli,
            cfg,
            context: Mutex::new(context),
            session: Mutex::new(session),
            permission,
            ask_tx,
            sandbox,
            status_signals,
            #[cfg(feature = "mcp")]
            mcp_manager: Mutex::new(None),
            events,
            mutable: Mutex::new(MutableState {
                seq: 0,
                cols: DEFAULT_COLS,
                line_count: initial_line_count,
                running: false,
                reasoning_enabled: true,
                reasoning_effort: None,
                abort_handle: None,
                turn: 0,
                #[cfg(feature = "loop")]
                loop_state: None,
                next_artifact_id: 1,
                next_permission_id: 1,
                pending_permissions: HashMap::new(),
                last_event_at: None,
            }),
            registry_dir: registration.dir.clone(),
            socket_path: registration.socket_path.clone(),
        });
        if let Some(permission) = &server.permission {
            let session = server.session.lock().await;
            permission
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .allow_session_tool_outputs(&session.id);
        }

        if let Some(ask_rx) = ask_rx {
            tokio::spawn(permission_pump(server.clone(), ask_rx));
        }
        let (live_output_tx, live_output_rx) = mpsc::channel::<BashLiveOutputRequest>(16);
        set_bash_live_output_sender(Some(live_output_tx));
        tokio::spawn(live_output_pump(server.clone(), live_output_rx));

        eprintln!(
            "zerostack Emacs session {}",
            server.current_session_id().await
        );
        eprintln!("socket {}", registration.socket_path.display());

        loop {
            let (stream, _) = listener.accept().await?;
            tokio::spawn(handle_client(server.clone(), stream));
        }
    }

    pub fn print_sessions() -> anyhow::Result<()> {
        let sessions = list_registered_sessions()?;
        if sessions.is_empty() {
            println!("no running Emacs sessions");
            return Ok(());
        }

        println!("running Emacs sessions ({}):", sessions.len());
        for meta in sessions {
            let time = format_time(&meta.updated_at);
            println!(
                "  {}  {}  pid:{}  {}  {}  {}",
                short_id(&meta.session_id),
                time,
                meta.pid,
                meta.model,
                meta.cwd,
                meta.socket,
            );
        }
        Ok(())
    }

    impl Registration {
        fn create(session: &Session) -> anyhow::Result<(Self, UnixListener)> {
            let root = sessions_root();
            ensure_private_dir(&runtime_root())?;
            ensure_private_dir(&root)?;

            let dir = root.join(session.id.as_str());
            let socket_path = dir.join("sock");
            if socket_alive(&socket_path) {
                anyhow::bail!(
                    "Emacs session {} is already running at {}",
                    session.id,
                    socket_path.display()
                );
            }
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            ensure_private_dir(&dir)?;

            let listener = UnixListener::bind(&socket_path)
                .with_context(|| format!("bind {}", socket_path.display()))?;
            let registration = Registration { dir, socket_path };
            registration.write_pid()?;
            registration.update_meta(session)?;
            Ok((registration, listener))
        }

        fn write_pid(&self) -> anyhow::Result<()> {
            std::fs::write(self.dir.join("pid"), std::process::id().to_string())?;
            Ok(())
        }

        fn update_meta(&self, session: &Session) -> anyhow::Result<()> {
            let meta = SessionMeta::from_session(session, &self.socket_path);
            write_meta_atomic(&self.dir, &meta)
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.socket_path);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl SessionMeta {
        fn from_session(session: &Session, socket_path: &Path) -> Self {
            SessionMeta {
                session_id: session.id.to_string(),
                pid: std::process::id(),
                cwd: session.working_dir.to_string(),
                model: session.model.to_string(),
                provider: session.provider.to_string(),
                created_at: session.created_at.to_string(),
                updated_at: session.updated_at.to_string(),
                title: session.title(),
                tokens: session.effective_context_tokens(),
                context_window: session.context_window,
                protocol: PROTOCOL_VERSION,
                socket: socket_path.to_string_lossy().to_string(),
                thinking: "on".to_string(),
                reasoning_effort_supported: false,
                reasoning_effort: None,
            }
        }
    }

    impl Server {
        async fn next_seq(&self) -> u64 {
            let mut mutable = self.mutable.lock().await;
            mutable.seq = mutable.seq.saturating_add(1);
            mutable.seq
        }

        async fn current_session_id(&self) -> String {
            self.session.lock().await.id.to_string()
        }

        async fn event_form(&self, event_type: &str, fields: String) -> String {
            let seq = self.next_seq().await;
            let session_id = self.current_session_id().await;
            format!(
                "(event :seq {} :session {} :type {}{})",
                seq,
                sexp_quote(&session_id),
                event_type,
                fields,
            )
        }

        async fn broadcast_event(&self, event_type: &str, fields: String) {
            let form = self.event_form(event_type, fields).await;
            let _ = self.events.send(form);
        }

        async fn send_render_event(
            &self,
            event_type: &str,
            turn: u64,
            replace_from: usize,
            lines: Vec<WireLine>,
        ) {
            let len = lines.len();
            let should_update_meta = len > 0;
            {
                let mut mutable = self.mutable.lock().await;
                mutable.line_count = replace_from.saturating_add(len);
                if should_update_meta {
                    mutable.last_event_at = Some(chrono::Utc::now().to_rfc3339());
                }
            }
            if should_update_meta {
                self.update_meta_from_session().await;
            }
            self.broadcast_event(
                event_type,
                format!(
                    " :turn {} :replace-from {} :lines {}",
                    turn,
                    replace_from,
                    lines_to_sexp(&lines),
                ),
            )
            .await;
        }

        async fn append_lines(&self, event_type: &str, turn: u64, lines: Vec<WireLine>) {
            let start = {
                let mutable = self.mutable.lock().await;
                mutable.line_count
            };
            self.send_render_event(event_type, turn, start, lines).await;
        }

        async fn update_meta_from_session(&self) {
            let session = self.session.lock().await;
            let mut meta = SessionMeta::from_session(&session, &self.socket_path);
            let mutable = self.mutable.lock().await;
            if let Some(last_event_at) = mutable.last_event_at.clone() {
                meta.updated_at = last_event_at;
            }
            meta.thinking = thinking_label(mutable.reasoning_enabled).to_string();
            apply_reasoning_effort_meta(&mut meta, &self.cfg, &mutable);
            if let Err(e) = write_meta_atomic(&self.registry_dir, &meta) {
                tracing::warn!("failed to update Emacs session metadata: {e}");
            }
        }

        async fn next_artifact_id(&self) -> u64 {
            let mut mutable = self.mutable.lock().await;
            let id = mutable.next_artifact_id;
            mutable.next_artifact_id = mutable.next_artifact_id.saturating_add(1);
            id
        }

        async fn create_artifact(
            &self,
            turn: u64,
            kind: &'static str,
            label: &str,
            contents: &str,
        ) -> anyhow::Result<ArtifactInfo> {
            let id = self.next_artifact_id().await;
            let filename = format!("{:04}-{}.txt", id, safe_filename(label));
            self.write_artifact_file(turn, kind, &filename, contents)
                .await
        }

        async fn write_artifact_file(
            &self,
            turn: u64,
            kind: &'static str,
            filename: &str,
            contents: &str,
        ) -> anyhow::Result<ArtifactInfo> {
            self.write_artifact_file_with_mime(
                turn,
                kind,
                filename,
                contents,
                "text/plain; charset=utf-8",
            )
            .await
        }

        async fn write_artifact_file_with_mime(
            &self,
            turn: u64,
            kind: &'static str,
            filename: &str,
            contents: &str,
            mime: &'static str,
        ) -> anyhow::Result<ArtifactInfo> {
            let dir = self
                .registry_dir
                .join("artifacts")
                .join(format!("turn-{turn}"));
            ensure_private_dir(&dir)?;
            let path = dir.join(filename);
            tokio::fs::write(&path, contents)
                .await
                .with_context(|| format!("write artifact {}", path.display()))?;
            Ok(ArtifactInfo {
                kind,
                path,
                mime,
                bytes: contents.len(),
                preview: preview_text(contents),
            })
        }
    }

    async fn handle_client(server: Arc<Server>, stream: tokio::net::UnixStream) {
        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader).lines();
        let (client_tx, mut client_rx) = mpsc::channel::<String>(128);

        let writer_task = tokio::spawn(async move {
            let mut writer = writer;
            while let Some(line) = client_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
            }
        });

        let mut event_rx = server.events.subscribe();
        let event_tx = client_tx.clone();
        let event_task = tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(line) => {
                        if event_tx.send(line).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let _ = client_tx
            .send(format!(
                "(ready :protocol {} :session {} :pid {} :socket {})",
                PROTOCOL_VERSION,
                sexp_quote(&server.current_session_id().await),
                std::process::id(),
                sexp_quote(server.socket_path.to_string_lossy().as_ref()),
            ))
            .await;

        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match parse_command(&line) {
                        Ok(cmd) => handle_command(server.clone(), cmd, &client_tx).await,
                        Err(e) => {
                            let _ = client_tx
                                .send(format!("(error :message {})", sexp_quote(&e.to_string())))
                                .await;
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = client_tx
                        .send(format!("(error :message {})", sexp_quote(&e.to_string())))
                        .await;
                    break;
                }
            }
        }

        event_task.abort();
        drop(client_tx);
        let _ = writer_task.await;
    }

    async fn handle_command(server: Arc<Server>, cmd: Command, out: &mpsc::Sender<String>) {
        let result = match cmd.name.as_str() {
            "hello" => handle_hello(&server, &cmd, out).await,
            "attach" | "render" => handle_attach(&server, &cmd, out).await,
            "set-view" => handle_set_view(&server, &cmd, out).await,
            "file-add" => handle_file_add(&server, &cmd, out).await,
            "file-drop" => handle_file_drop(&server, &cmd, out).await,
            "file-drop-all" => handle_file_drop_all(&server, &cmd, out).await,
            "file-list" => handle_file_list(&server, &cmd, out).await,
            "prompt" => handle_prompt(server.clone(), &cmd, out).await,
            "compact" => handle_compact(&server, &cmd, out).await,
            "fork" => handle_fork(&server, &cmd, out).await,
            "provider" => handle_provider(&server, &cmd, out).await,
            "model" => handle_model(&server, &cmd, out).await,
            "mcp" => handle_mcp(&server, &cmd, out).await,
            "thinking" | "reasoning" => handle_thinking(&server, &cmd, out).await,
            "loop-start" => handle_loop_start(server.clone(), &cmd, out).await,
            "loop-stop" => handle_loop_stop(&server, &cmd, out).await,
            "loop-status" => handle_loop_status(&server, &cmd, out).await,
            "abort" => handle_abort(&server, &cmd, out).await,
            "permission-answer" => handle_permission_answer(&server, &cmd, out).await,
            "list-sessions" => handle_list_sessions(&cmd, out).await,
            "status" => handle_status(&server, &cmd, out).await,
            _ => Err(anyhow::anyhow!("unknown command '{}'", cmd.name)),
        };

        if let Err(e) = result {
            send_error(out, request_arg(&cmd), &e.to_string()).await;
        }
    }

    async fn handle_hello(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        if let Some(cols) = usize_arg(cmd, "cols") {
            server.mutable.lock().await.cols = cols.max(20);
        }
        let cols = server.mutable.lock().await.cols;
        send_ok(
            out,
            request_arg(cmd),
            format!(
                " :protocol {} :session {} :pid {} :cols {} :socket {}",
                PROTOCOL_VERSION,
                sexp_quote(&server.current_session_id().await),
                std::process::id(),
                cols,
                sexp_quote(server.socket_path.to_string_lossy().as_ref()),
            ),
        )
        .await;
        Ok(())
    }

    async fn handle_attach(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        if let Some(cols) = usize_arg(cmd, "cols") {
            server.mutable.lock().await.cols = cols.max(20);
        }
        let cols = server.mutable.lock().await.cols;
        let lines = {
            let session = server.session.lock().await;
            let context = server.context.lock().await;
            render_session_lines(&session, &server.cli, &server.cfg, &context, cols)
        };
        let running = server.mutable.lock().await.running;
        if !running {
            server.mutable.lock().await.line_count = lines.len();
        }
        let seq = server.next_seq().await;
        out.send(format!(
            "(event :seq {} :session {} :type session-render :replace-from 0 :lines {})",
            seq,
            sexp_quote(&server.current_session_id().await),
            lines_to_sexp(&lines),
        ))
        .await?;
        Ok(())
    }

    async fn handle_set_view(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let cols = usize_arg(cmd, "cols").unwrap_or(DEFAULT_COLS).max(20);
        server.mutable.lock().await.cols = cols;
        send_ok(out, request_arg(cmd), format!(" :cols {}", cols)).await;
        Ok(())
    }

    async fn handle_file_add(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let raw_path = string_arg(cmd, "path")
            .or_else(|| string_arg(cmd, "file"))
            .context("file-add requires :path")?;
        let path = resolve_session_path(server, &raw_path).await;
        if !path.exists() {
            anyhow::bail!("file not found: {}", path.display());
        }
        if !path.is_file() {
            anyhow::bail!("not a file: {}", path.display());
        }

        let outcome = add_attachment_path(server, path).await?;
        send_ok(out, request_arg(cmd), outcome.fields()).await;
        Ok(())
    }

    async fn handle_file_drop(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let outcome = if let Some(index) = usize_arg(cmd, "index") {
            drop_attachment_index(server, index).await?
        } else {
            let raw_path = string_arg(cmd, "path")
                .or_else(|| string_arg(cmd, "file"))
                .context("file-drop requires :path or :index")?;
            let path = resolve_session_path(server, &raw_path).await;
            drop_attachment_path(server, path).await?
        };
        send_ok(out, request_arg(cmd), outcome.fields()).await;
        Ok(())
    }

    async fn handle_file_drop_all(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let files = {
            let mut context = server.context.lock().await;
            let count = context.extra_files.len();
            context.extra_files.clear();
            count
        };
        let media = clear_pending_media(server).await;
        send_ok(
            out,
            request_arg(cmd),
            format!(
                " :files {} :media {} :message {}",
                files,
                media,
                sexp_quote(&format!(
                    "dropped {files} file(s), {media} media attachment(s)"
                )),
            ),
        )
        .await;
        Ok(())
    }

    async fn handle_file_list(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let items = attachment_items(server).await;
        let total = items.len();
        send_ok(
            out,
            request_arg(cmd),
            format!(
                " :count {} :items {} :message {}",
                total,
                attachment_items_to_sexp(&items),
                sexp_quote(&attachment_list_message(&items)),
            ),
        )
        .await;
        Ok(())
    }

    async fn handle_provider(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        ensure_idle_for_switch(server, "provider").await?;
        let raw_provider = string_arg(cmd, "provider")
            .or_else(|| atom_arg(cmd, "provider"))
            .context("provider requires :provider")?;
        config::commands::validate_provider(&server.cfg, &raw_provider)?;
        let provider = config::commands::canonical_provider_name(&raw_provider);
        let model = if let Some((model, _)) =
            crate::provider::default_model_for_provider(&provider, &server.cfg)
        {
            model
        } else {
            let session = server.session.lock().await;
            session.model.to_string()
        };
        let session_id = {
            let session = server.session.lock().await;
            session.id.clone()
        };

        let client = crate::provider::create_client(
            &provider,
            server.cli.api_key.as_deref(),
            &server.cfg.custom_providers_map(),
            server.cfg.api_keys.as_ref(),
            Some(session_id.as_str()),
        )?;

        ensure_idle_for_switch(server, "provider").await?;
        {
            let mut current = server.client.lock().await;
            *current = client.clone();
        }
        update_session_provider_model(server, &provider, &model).await?;
        clear_unsupported_reasoning_effort(server).await;
        sync_subagent_with_main(server, &client, &provider, &model).await;

        let message = format!("switched to provider: {provider} (model: {model})");
        send_ok(
            out,
            request_arg(cmd),
            format!(
                " :provider {} :model {} :message {}",
                sexp_quote(&provider),
                sexp_quote(&model),
                sexp_quote(&message),
            ),
        )
        .await;
        Ok(())
    }

    async fn handle_thinking(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        ensure_idle_for_switch(server, "thinking").await?;
        let level = string_arg(cmd, "level")
            .or_else(|| atom_arg(cmd, "level"))
            .unwrap_or_else(|| "toggle".to_string());
        if crate::provider::valid_reasoning_effort(&level) {
            let mut mutable = server.mutable.lock().await;
            let (provider, model) = current_provider_model(server).await;
            let Some(effort) =
                crate::provider::normalize_reasoning_effort_value(&provider, &model, &level)
            else {
                anyhow::bail!("reasoning effort '{level}' is not supported by {provider}/{model}");
            };
            mutable.reasoning_effort = Some(CompactString::new(effort));
            let label = thinking_label(mutable.reasoning_enabled);
            let effort = mutable.reasoning_effort.as_deref().unwrap_or(effort);
            send_ok(
                out,
                request_arg(cmd),
                format!(
                    " :thinking {} :reasoning-effort-supported t :reasoning-effort {} :message {}",
                    sexp_quote(label),
                    sexp_quote(effort),
                    sexp_quote(&format!("reasoning effort: {effort}")),
                ),
            )
            .await;
            return Ok(());
        }
        let enabled = match level.as_str() {
            "on" | "true" | "t" | "enabled" => true,
            "off" | "false" | "nil" | "disabled" => false,
            "toggle" => !server.mutable.lock().await.reasoning_enabled,
            other => anyhow::bail!(
                "unknown thinking level '{}'; use on, off, none, minimal, low, medium, high, xhigh, or max",
                other
            ),
        };
        server.mutable.lock().await.reasoning_enabled = enabled;
        let label = thinking_label(enabled);
        send_ok(
            out,
            request_arg(cmd),
            format!(
                " :thinking {} :message {}",
                sexp_quote(label),
                sexp_quote(&format!("thinking: {label}")),
            ),
        )
        .await;
        Ok(())
    }

    async fn handle_mcp(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        #[cfg(not(feature = "mcp"))]
        {
            send_ok(
                out,
                request_arg(cmd),
                format!(" :message {}", sexp_quote("MCP support not enabled")),
            )
            .await;
            return Ok(());
        }

        #[cfg(feature = "mcp")]
        {
            let Some(configs) = server.cfg.mcp_servers.as_ref() else {
                send_ok(
                    out,
                    request_arg(cmd),
                    format!(" :message {}", sexp_quote("no MCP servers configured")),
                )
                .await;
                return Ok(());
            };
            if configs.is_empty() {
                send_ok(
                    out,
                    request_arg(cmd),
                    format!(" :message {}", sexp_quote("no MCP servers configured")),
                )
                .await;
                return Ok(());
            }

            let mut guard = server.mcp_manager.lock().await;
            if guard.is_none() {
                *guard = Some(crate::extras::mcp::McpClientManager::connect_all(configs).await);
            }
            let Some(manager) = guard.as_ref() else {
                return Ok(());
            };
            if manager.handles.is_empty() {
                send_ok(
                    out,
                    request_arg(cmd),
                    format!(" :message {}", sexp_quote("no MCP servers connected")),
                )
                .await;
                return Ok(());
            }

            let mut lines = vec!["MCP servers:".to_string()];
            for handle in &manager.handles {
                match handle.list_tools().await {
                    Ok(tools) => {
                        lines.push(format!("{}: {} tool(s)", handle.server_name, tools.len()));
                        for tool in tools {
                            let description = tool.description.unwrap_or_default();
                            if description.is_empty() {
                                lines.push(format!("  - {}", tool.name));
                            } else {
                                lines.push(format!("  - {} — {}", tool.name, description));
                            }
                        }
                    }
                    Err(e) => {
                        lines.push(format!("{}: failed to list tools: {e}", handle.server_name))
                    }
                }
            }
            send_ok(
                out,
                request_arg(cmd),
                format!(" :message {}", sexp_quote(&lines.join("\n"))),
            )
            .await;
            Ok(())
        }
    }

    async fn handle_model(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        ensure_idle_for_switch(server, "model").await?;
        let model = string_arg(cmd, "model")
            .or_else(|| atom_arg(cmd, "model"))
            .context("model requires :model")?;
        if model.trim().is_empty() {
            anyhow::bail!("model cannot be empty");
        }
        let provider = {
            let session = server.session.lock().await;
            session.provider.to_string()
        };
        update_session_provider_model(server, &provider, &model).await?;
        clear_unsupported_reasoning_effort(server).await;
        let client = server.client.lock().await.clone();
        sync_subagent_with_main(server, &client, &provider, &model).await;

        let message = format!("switched to model: {model}");
        send_ok(
            out,
            request_arg(cmd),
            format!(
                " :provider {} :model {} :message {}",
                sexp_quote(&provider),
                sexp_quote(&model),
                sexp_quote(&message),
            ),
        )
        .await;
        Ok(())
    }

    async fn ensure_idle_for_switch(server: &Arc<Server>, kind: &str) -> anyhow::Result<()> {
        let mutable = server.mutable.lock().await;
        if mutable.running {
            anyhow::bail!("cannot switch {kind} while a prompt is running");
        }
        #[cfg(feature = "loop")]
        if mutable
            .loop_state
            .as_ref()
            .map(|state| state.active)
            .unwrap_or(false)
        {
            anyhow::bail!("cannot switch {kind} while a loop is active");
        }
        Ok(())
    }

    async fn update_session_provider_model(
        server: &Arc<Server>,
        provider: &str,
        model: &str,
    ) -> anyhow::Result<()> {
        {
            let mut session = server.session.lock().await;
            session.provider = CompactString::new(provider);
            session.model = CompactString::new(model);
            session.update_context_window(server.cfg.resolve_context_window(provider, model));
            apply_quick_model_costs(&mut session, &server.cfg, provider, model);
            session.reset_calibration();
            session.updated_at = CompactString::new(chrono::Utc::now().to_rfc3339());
            if !server.cli.no_session {
                crate::session::storage::save_session(&session)?;
            }
        }
        server.update_meta_from_session().await;
        Ok(())
    }

    fn apply_quick_model_costs(session: &mut Session, cfg: &Config, provider: &str, model: &str) {
        let qm = config::quick_models_map(cfg);
        if let Some(q) = qm
            .values()
            .find(|q| q.provider.as_str() == provider && q.model.as_str() == model)
        {
            session.input_token_cost = q.input_token_cost;
            session.output_token_cost = q.output_token_cost;
        } else {
            session.input_token_cost = 0.0;
            session.output_token_cost = 0.0;
        }
    }

    #[cfg(feature = "subagents")]
    async fn sync_subagent_with_main(
        server: &Arc<Server>,
        client: &AnyClient,
        provider: &str,
        model: &str,
    ) {
        use crate::extras::subagents;

        if server.cfg.subagent_model.is_some() {
            return;
        }

        let sub_provider = server
            .cfg
            .subagent_provider
            .as_deref()
            .unwrap_or(provider)
            .to_string();
        let sub_model = server
            .cfg
            .subagent_model
            .as_deref()
            .unwrap_or(model)
            .to_string();

        if sub_provider == client.provider_name() {
            subagents::set_client_and_model(client.clone(), sub_model);
            return;
        }

        match crate::provider::create_client(
            &sub_provider,
            server.cli.api_key.as_deref(),
            &server.cfg.custom_providers_map(),
            server.cfg.api_keys.as_ref(),
            None,
        ) {
            Ok(client) => subagents::set_client_and_model(client, sub_model),
            Err(e) => tracing::warn!(
                "Could not propagate Emacs provider/model switch to subagent provider '{}' ({}); keeping previous subagent config",
                sub_provider,
                e,
            ),
        }
    }

    #[cfg(not(feature = "subagents"))]
    async fn sync_subagent_with_main(
        _server: &Arc<Server>,
        _client: &AnyClient,
        _provider: &str,
        _model: &str,
    ) {
    }

    async fn resolve_session_path(server: &Arc<Server>, raw_path: &str) -> PathBuf {
        let path = PathBuf::from(raw_path);
        if path.is_absolute() {
            path
        } else {
            let cwd = {
                let session = server.session.lock().await;
                PathBuf::from(session.working_dir.as_str())
            };
            cwd.join(path)
        }
    }

    async fn add_attachment_path(
        server: &Arc<Server>,
        path: PathBuf,
    ) -> anyhow::Result<AttachmentOutcome> {
        let canonical = path.canonicalize().unwrap_or(path);

        #[cfg(feature = "multimodal")]
        if crate::extras::multimodal::detect_media(&canonical).is_some() {
            let attachment = crate::extras::multimodal::load_attachment(&canonical)
                .with_context(|| format!("failed to load media: {}", canonical.display()))?;
            let bytes = attachment.size() as u64;
            let (kind, mime) = media_kind_mime(&attachment);
            let mime = mime.to_string();
            {
                let mut session = server.session.lock().await;
                session.pending_media.push(attachment);
            }
            return Ok(AttachmentOutcome {
                kind,
                path: canonical.clone(),
                bytes,
                mime: Some(mime),
                message: format!(
                    "attached {kind}: {} ({})",
                    canonical.display(),
                    format_bytes(bytes as usize)
                ),
            });
        }

        let bytes = std::fs::metadata(&canonical).map(|m| m.len()).unwrap_or(0);
        let already_added = {
            let mut context = server.context.lock().await;
            if context.extra_files.contains(&canonical) {
                true
            } else {
                context.extra_files.push(canonical.clone());
                false
            }
        };
        Ok(AttachmentOutcome {
            kind: "context-file",
            path: canonical.clone(),
            bytes,
            mime: None,
            message: if already_added {
                format!("already attached: {}", canonical.display())
            } else {
                format!(
                    "attached file: {} ({})",
                    canonical.display(),
                    format_bytes(bytes as usize)
                )
            },
        })
    }

    async fn drop_attachment_path(
        server: &Arc<Server>,
        path: PathBuf,
    ) -> anyhow::Result<AttachmentOutcome> {
        let canonical = path.canonicalize().unwrap_or(path);
        let bytes = std::fs::metadata(&canonical).map(|m| m.len()).unwrap_or(0);
        {
            let mut context = server.context.lock().await;
            if let Some(index) = context
                .extra_files
                .iter()
                .position(|item| item == &canonical)
            {
                context.extra_files.remove(index);
                return Ok(AttachmentOutcome {
                    kind: "context-file",
                    path: canonical.clone(),
                    bytes,
                    mime: None,
                    message: format!("dropped file: {}", canonical.display()),
                });
            }
        }

        #[cfg(feature = "multimodal")]
        {
            let mut session = server.session.lock().await;
            if let Some(index) = session
                .pending_media
                .iter()
                .position(|item| item.path() == canonical.as_path())
            {
                let attachment = session.pending_media.remove(index);
                let (kind, mime) = media_kind_mime(&attachment);
                return Ok(AttachmentOutcome {
                    kind,
                    path: canonical.clone(),
                    bytes: attachment.size() as u64,
                    mime: Some(mime.to_string()),
                    message: format!("dropped {kind}: {}", canonical.display()),
                });
            }
        }

        anyhow::bail!("not attached: {}", canonical.display())
    }

    async fn drop_attachment_index(
        server: &Arc<Server>,
        index: usize,
    ) -> anyhow::Result<AttachmentOutcome> {
        let context_count = server.context.lock().await.extra_files.len();
        if index < context_count {
            let path = {
                let mut context = server.context.lock().await;
                context.extra_files.remove(index)
            };
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            return Ok(AttachmentOutcome {
                kind: "context-file",
                path: path.clone(),
                bytes,
                mime: None,
                message: format!("dropped file: {}", path.display()),
            });
        }

        #[cfg(feature = "multimodal")]
        {
            let media_index = index.saturating_sub(context_count);
            let mut session = server.session.lock().await;
            if media_index < session.pending_media.len() {
                let attachment = session.pending_media.remove(media_index);
                let (kind, mime) = media_kind_mime(&attachment);
                return Ok(AttachmentOutcome {
                    kind,
                    path: attachment.path().to_path_buf(),
                    bytes: attachment.size() as u64,
                    mime: Some(mime.to_string()),
                    message: format!("dropped {kind}: {}", attachment.path().display()),
                });
            }
        }

        anyhow::bail!("no attachment at index {index}")
    }

    async fn clear_pending_media(server: &Arc<Server>) -> usize {
        #[cfg(feature = "multimodal")]
        {
            let mut session = server.session.lock().await;
            let count = session.pending_media.len();
            session.pending_media.clear();
            count
        }
        #[cfg(not(feature = "multimodal"))]
        {
            let _ = server;
            0
        }
    }

    async fn attachment_items(server: &Arc<Server>) -> Vec<AttachmentItem> {
        let mut items = Vec::new();
        {
            let context = server.context.lock().await;
            for path in &context.extra_files {
                let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                items.push(AttachmentItem {
                    index: items.len(),
                    kind: "context-file",
                    path: path.clone(),
                    bytes,
                    mime: None,
                });
            }
        }
        #[cfg(feature = "multimodal")]
        {
            let session = server.session.lock().await;
            for media in &session.pending_media {
                let (kind, mime) = media_kind_mime(media);
                items.push(AttachmentItem {
                    index: items.len(),
                    kind,
                    path: media.path().to_path_buf(),
                    bytes: media.size() as u64,
                    mime: Some(mime.to_string()),
                });
            }
        }
        items
    }

    fn attachment_list_message(items: &[AttachmentItem]) -> String {
        if items.is_empty() {
            return "no attached files".to_string();
        }
        items
            .iter()
            .map(|item| {
                let mime = item
                    .mime
                    .as_deref()
                    .map(|mime| format!(", {mime}"))
                    .unwrap_or_default();
                format!(
                    "{}:{} {} ({}{})",
                    item.index,
                    item.kind,
                    item.path.display(),
                    format_bytes(item.bytes as usize),
                    mime,
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    #[cfg(feature = "multimodal")]
    fn media_kind_mime(
        attachment: &crate::extras::multimodal::MediaAttachment,
    ) -> (&'static str, &str) {
        match attachment {
            crate::extras::multimodal::MediaAttachment::Image { mime, .. } => {
                ("image", mime.as_str())
            }
            crate::extras::multimodal::MediaAttachment::Audio { mime, .. } => {
                ("audio", mime.as_str())
            }
            crate::extras::multimodal::MediaAttachment::Document { mime, .. } => {
                ("document", mime.as_str())
            }
        }
    }

    async fn handle_prompt(
        server: Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let text = string_arg(cmd, "text").context("prompt requires :text")?;
        let turn = {
            let mut mutable = server.mutable.lock().await;
            if mutable.running {
                anyhow::bail!("agent is already running");
            }
            #[cfg(feature = "loop")]
            if mutable.loop_state.as_ref().is_some_and(|ls| ls.active) {
                anyhow::bail!("loop is active; stop it before sending a one-off prompt");
            }
            mutable.running = true;
            mutable.turn = mutable.turn.saturating_add(1);
            mutable.turn
        };
        tokio::spawn(run_prompt(server, text, turn));
        send_ok(out, request_arg(cmd), format!(" :turn {}", turn)).await;
        Ok(())
    }

    #[cfg(feature = "loop")]
    async fn handle_loop_start(
        server: Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let prompt = string_arg(cmd, "prompt")
            .or_else(|| string_arg(cmd, "text"))
            .context("loop-start requires :prompt")?;
        if prompt.trim().is_empty() {
            anyhow::bail!("loop-start requires a non-empty :prompt");
        }

        let plan_file = string_arg(cmd, "plan")
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(crate::extras::r#loop::DEFAULT_PLAN_FILENAME));
        let max_iterations = u64_arg(cmd, "max").map(|value| value as u32);
        let run_cmd = string_arg(cmd, "run").filter(|value| !value.trim().is_empty());

        let (turn, iteration_prompt, fields) = {
            let mut mutable = server.mutable.lock().await;
            if mutable.running {
                anyhow::bail!("agent is already running");
            }
            if mutable.loop_state.as_ref().is_some_and(|ls| ls.active) {
                anyhow::bail!("loop is already active");
            }

            let mut loop_state =
                crate::extras::r#loop::LoopState::new(prompt, plan_file, max_iterations, run_cmd);
            loop_state.iteration = 1;
            let iteration_prompt = loop_state.build_prompt();
            let fields = loop_fields(&loop_state);

            mutable.loop_state = Some(loop_state);
            mutable.running = true;
            mutable.turn = mutable.turn.saturating_add(1);
            (mutable.turn, iteration_prompt, fields)
        };

        server
            .broadcast_event("loop-started", format!(" :turn {}{}", turn, fields))
            .await;
        server
            .broadcast_event("loop-iteration", format!(" :turn {}{}", turn, fields))
            .await;
        tokio::spawn(run_loop(server, iteration_prompt, turn));
        send_ok(out, request_arg(cmd), format!(" :turn {}{}", turn, fields)).await;
        Ok(())
    }

    #[cfg(not(feature = "loop"))]
    async fn handle_loop_start(
        _server: Arc<Server>,
        _cmd: &Command,
        _out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("loop support is not enabled in this build")
    }

    async fn handle_loop_stop(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let stopped = stop_loop_state(server, true).await;
        send_ok(
            out,
            request_arg(cmd),
            format!(" :stopped {}", bool_atom(stopped)),
        )
        .await;
        Ok(())
    }

    #[cfg(feature = "loop")]
    async fn handle_loop_status(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let fields = {
            let mutable = server.mutable.lock().await;
            match mutable.loop_state.as_ref() {
                Some(loop_state) => loop_fields(loop_state),
                None => " :active nil".to_string(),
            }
        };
        send_ok(out, request_arg(cmd), fields).await;
        Ok(())
    }

    #[cfg(not(feature = "loop"))]
    async fn handle_loop_status(
        _server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        send_ok(out, request_arg(cmd), " :active nil".to_string()).await;
        Ok(())
    }

    async fn handle_fork(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let message_index = usize_arg(cmd, "index").context("fork requires :index")?;
        let (old_id, new_id, message_count) = {
            let mutable = server.mutable.lock().await;
            if mutable.running {
                anyhow::bail!("cannot fork while an agent turn is running");
            }
            #[cfg(feature = "loop")]
            if mutable.loop_state.as_ref().is_some_and(|ls| ls.active) {
                anyhow::bail!("cannot fork while a loop is active");
            }
            let mut session = server.session.lock().await;
            if message_index > session.messages.len() {
                anyhow::bail!("message index out of range");
            }
            let old_id = session.id.to_string();
            *session = session.fork_before_message(message_index);
            if !server.cli.no_session {
                crate::session::storage::save_session(&session)?;
            }
            (old_id, session.id.to_string(), session.messages.len())
        };
        server.update_meta_from_session().await;
        let cols = server.mutable.lock().await.cols;
        let lines = {
            let session = server.session.lock().await;
            let context = server.context.lock().await;
            render_session_lines(&session, &server.cli, &server.cfg, &context, cols)
        };
        server
            .send_render_event("session-render", 0, 0, lines)
            .await;
        send_ok(
            out,
            request_arg(cmd),
            format!(
                " :old-session {} :new-session {} :index {} :messages {} :message {}",
                sexp_quote(&old_id),
                sexp_quote(&new_id),
                message_index,
                message_count,
                sexp_quote(&format!(
                    "forked {} -> {} before message {}",
                    short_id(&old_id),
                    short_id(&new_id),
                    message_index,
                )),
            ),
        )
        .await;
        Ok(())
    }

    async fn handle_compact(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let turn = {
            let mut mutable = server.mutable.lock().await;
            if mutable.running {
                anyhow::bail!("cannot compact while an agent turn is running");
            }
            mutable.running = true;
            mutable.turn = mutable.turn.saturating_add(1);
            mutable.turn
        };
        if let Some(ss) = server.status_signals.as_ref() {
            ss.send_start();
        }
        server
            .broadcast_event("compact-started", format!(" :turn {}", turn))
            .await;

        let instructions = string_arg(cmd, "instructions");
        let outcome = compact_session(server, instructions.as_deref()).await;
        {
            let mut mutable = server.mutable.lock().await;
            if mutable.turn == turn {
                mutable.running = false;
            }
        }
        if let Some(ss) = server.status_signals.as_ref() {
            ss.send_stop();
        }

        if outcome.is_err() {
            server
                .broadcast_event("compact-done", format!(" :turn {}", turn))
                .await;
        }
        let outcome = outcome?;
        let cols = server.mutable.lock().await.cols;
        let lines = {
            let session = server.session.lock().await;
            let context = server.context.lock().await;
            render_session_lines(&session, &server.cli, &server.cfg, &context, cols)
        };
        server
            .send_render_event("session-render", turn, 0, lines)
            .await;
        server
            .broadcast_event("compact-done", format!(" :turn {}", turn))
            .await;

        send_ok(out, request_arg(cmd), compact_outcome_fields(&outcome)).await;
        Ok(())
    }

    fn compact_outcome_fields(outcome: &CompactionOutcome) -> String {
        format!(
            " :compacted {} :messages {} :saved-tokens {} :message {}",
            bool_atom(outcome.compacted),
            outcome.messages,
            outcome.saved_tokens,
            sexp_quote(&outcome.message),
        )
    }

    async fn compact_session(
        server: &Arc<Server>,
        instructions: Option<&str>,
    ) -> anyhow::Result<CompactionOutcome> {
        let qm = crate::config::quick_models_map(&server.cfg);
        let plan = {
            let session = server.session.lock().await;
            let reserve = server.cfg.resolve_reserve_tokens(&session.model, &qm);
            let keep_recent = server.cfg.resolve_keep_recent_tokens();
            let max_tokens = session.context_window.saturating_sub(reserve);

            if session.effective_context_tokens() <= max_tokens {
                return Ok(CompactionOutcome {
                    compacted: false,
                    messages: 0,
                    saved_tokens: 0,
                    message: "context within limits, no compression needed".to_string(),
                });
            }

            let cut_idx = Session::select_compaction_cut(&session.messages, keep_recent);
            if cut_idx == 0 {
                return Ok(CompactionOutcome {
                    compacted: false,
                    messages: 0,
                    saved_tokens: 0,
                    message: "nothing to compress (entire context is recent)".to_string(),
                });
            }

            let messages = session.messages[..cut_idx].to_vec();
            let previous_summary = session.compactions.last().map(|c| c.summary.to_string());
            let saved_tokens = messages.iter().map(|m| m.estimated_tokens).sum::<u64>();
            (
                session.model.to_string(),
                cut_idx,
                messages,
                previous_summary,
                saved_tokens,
            )
        };

        let (model, cut_idx, messages, previous_summary, saved_tokens) = plan;
        let client = server.client.lock().await.clone();
        let summary = client
            .compress_messages(&model, &messages, previous_summary.as_deref(), instructions)
            .await?;

        {
            let mut session = server.session.lock().await;
            session.compress(summary, cut_idx, saved_tokens);
            if !server.cli.no_session {
                crate::session::storage::save_session(&session)?;
            }
        }
        server.update_meta_from_session().await;

        Ok(CompactionOutcome {
            compacted: true,
            messages: cut_idx,
            saved_tokens,
            message: format!("compressed {cut_idx} messages (saved ~{saved_tokens} tokens)"),
        })
    }

    async fn handle_abort(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        stop_loop_state(server, false).await;
        let aborted = {
            let mut mutable = server.mutable.lock().await;
            let was_running = mutable.running;
            if let Some(handle) = mutable.abort_handle.take() {
                handle.abort();
            }
            mutable.running = false;
            was_running
        };
        server.sandbox.kill_active();
        if aborted {
            if let Some(ss) = server.status_signals.as_ref() {
                ss.send_stop();
            }
            server.broadcast_event("aborted", "".to_string()).await;
        }
        send_ok(
            out,
            request_arg(cmd),
            format!(" :aborted {}", bool_atom(aborted)),
        )
        .await;
        Ok(())
    }

    #[cfg(feature = "loop")]
    async fn stop_loop_state(server: &Arc<Server>, abort_running: bool) -> bool {
        let (stopped, aborted, abort_handle) = {
            let mut mutable = server.mutable.lock().await;
            let stopped = mutable.loop_state.take().is_some();
            let aborted = abort_running && mutable.running;
            let abort_handle = if abort_running {
                mutable.abort_handle.take()
            } else {
                None
            };
            if abort_running {
                mutable.running = false;
            }
            (stopped, aborted, abort_handle)
        };
        if let Some(handle) = abort_handle {
            handle.abort();
        }
        if aborted {
            server.sandbox.kill_active();
            if let Some(ss) = server.status_signals.as_ref() {
                ss.send_stop();
            }
            server.broadcast_event("aborted", "".to_string()).await;
        }
        if stopped {
            server
                .broadcast_event("loop-stopped", " :reason stopped".to_string())
                .await;
        }
        stopped
    }

    #[cfg(not(feature = "loop"))]
    async fn stop_loop_state(_server: &Arc<Server>, _abort_running: bool) -> bool {
        false
    }

    async fn handle_permission_answer(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let request = u64_arg(cmd, "request").context("permission-answer requires :request")?;
        let decision = atom_arg(cmd, "decision").context("permission-answer requires :decision")?;
        let ask_req = {
            let mut mutable = server.mutable.lock().await;
            mutable.pending_permissions.remove(&request)
        }
        .context("unknown permission request")?;

        let user_decision = match decision.as_str() {
            "allow-once" => UserDecision::AllowOnce,
            "allow-always" => {
                let pattern = string_arg(cmd, "pattern")
                    .unwrap_or_else(|| suggest_pattern(&ask_req.tool, &ask_req.input));
                {
                    let mut session = server.session.lock().await;
                    session.permission_allowlist.push(PermissionAllowEntry {
                        tool: ask_req.tool.clone(),
                        pattern: CompactString::from(pattern.clone()),
                    });
                    if !server.cli.no_session {
                        crate::session::storage::save_session(&session)?;
                    }
                }
                server.update_meta_from_session().await;
                UserDecision::AllowAlways(pattern)
            }
            "deny" => UserDecision::Deny,
            other => anyhow::bail!("unknown permission decision '{}'", other),
        };
        let _ = ask_req.reply.send(user_decision);
        server
            .broadcast_event(
                "permission-answered",
                format!(" :request {} :decision {}", request, decision),
            )
            .await;
        send_ok(out, request_arg(cmd), format!(" :request {}", request)).await;
        Ok(())
    }

    async fn handle_list_sessions(cmd: &Command, out: &mpsc::Sender<String>) -> anyhow::Result<()> {
        let limit = usize_arg(cmd, "limit").unwrap_or(usize::MAX);
        let sessions = list_registered_sessions()?;
        let rendered = sessions
            .iter()
            .take(limit)
            .map(meta_to_sexp)
            .collect::<Vec<_>>()
            .join(" ");
        out.send(format!(
            "(sessions :request {} :items ({}))",
            request_arg(cmd).unwrap_or_else(|| "nil".to_string()),
            rendered,
        ))
        .await?;
        Ok(())
    }

    async fn handle_status(
        server: &Arc<Server>,
        cmd: &Command,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let mut meta = {
            let session = server.session.lock().await;
            SessionMeta::from_session(&session, &server.socket_path)
        };
        {
            let mutable = server.mutable.lock().await;
            meta.thinking = thinking_label(mutable.reasoning_enabled).to_string();
            apply_reasoning_effort_meta(&mut meta, &server.cfg, &mutable);
        }
        out.send(format!(
            "(status :request {} :session {})",
            request_arg(cmd).unwrap_or_else(|| "nil".to_string()),
            meta_to_sexp(&meta),
        ))
        .await?;
        Ok(())
    }

    async fn run_prompt(server: Arc<Server>, text: String, turn: u64) {
        let _ = run_prompt_once(server, text, turn).await;
    }

    fn tool_result_artifact_content(
        session: &mut Session,
        name: &str,
        output: &str,
    ) -> CompactString {
        sanitize_output(&session.add_tool_result(name, output))
    }

    async fn run_prompt_once(
        server: Arc<Server>,
        text: String,
        turn: u64,
    ) -> (bool, Option<String>) {
        let response = match run_prompt_inner(server.clone(), text, turn).await {
            Ok(response) => response,
            Err(e) => {
                server
                    .broadcast_event("error", format!(" :message {}", sexp_quote(&e.to_string())))
                    .await;
                None
            }
        };
        let active_before_cleanup = server.sandbox.active_group_count();
        if active_before_cleanup > 0 {
            server.sandbox.kill_active();
        }
        let cleared_current_turn = {
            let mut mutable = server.mutable.lock().await;
            if mutable.turn == turn {
                mutable.running = false;
                mutable.abort_handle = None;
                true
            } else {
                false
            }
        };
        if cleared_current_turn && let Some(ss) = server.status_signals.as_ref() {
            ss.send_stop();
        }
        (cleared_current_turn, response)
    }

    #[cfg(feature = "loop")]
    async fn run_loop(server: Arc<Server>, mut prompt: String, mut turn: u64) {
        loop {
            let (cleared_current_turn, response) =
                run_prompt_once(server.clone(), prompt, turn).await;
            if !cleared_current_turn {
                break;
            }

            let Some(response) = response else {
                stop_loop_state(&server, false).await;
                break;
            };

            match continue_loop_after_turn(server.clone(), turn, response).await {
                NextLoop::Start {
                    turn: next_turn,
                    prompt: next_prompt,
                    fields,
                } => {
                    server
                        .broadcast_event(
                            "loop-iteration",
                            format!(" :turn {}{}", next_turn, fields),
                        )
                        .await;
                    turn = next_turn;
                    prompt = next_prompt;
                }
                NextLoop::Stopped { reason } => {
                    server
                        .broadcast_event("loop-stopped", format!(" :reason {}", reason))
                        .await;
                    break;
                }
                NextLoop::None => break,
            }
        }
    }

    async fn run_prompt_inner(
        server: Arc<Server>,
        text: String,
        turn: u64,
    ) -> anyhow::Result<Option<String>> {
        let (history, user_index, assistant_index) = {
            let mut session = server.session.lock().await;
            let user_index = session.messages.len();
            let history = convert_history(&session);
            #[cfg(feature = "multimodal")]
            let history = {
                let media = session.drain_media();
                if !media.is_empty() {
                    let mut history = history;
                    history.extend(crate::agent::runner::media_to_messages(&media));
                    history
                } else {
                    history
                }
            };
            session.add_message(MessageRole::User, &text);
            let assistant_index = session.messages.len();
            if !server.cli.no_session {
                let _ = crate::session::chat_history::append_entry(
                    &crate::session::chat_history::ChatHistoryEntry {
                        content: text.clone(),
                        timestamp: session.updated_at.clone(),
                    },
                );
            }
            (history, user_index, assistant_index)
        };
        server
            .append_lines(
                "user-render",
                turn,
                with_source_lines(render_user_lines(&text), user_index, MessageRole::User),
            )
            .await;
        server.update_meta_from_session().await;

        if let Some(ss) = server.status_signals.as_ref() {
            ss.send_start();
        }

        let client = server.client.lock().await.clone();
        let model = client.completion_model({
            let session = server.session.lock().await;
            session.model.to_string()
        });
        let (temperature, reasoning_effort) = {
            let session = server.session.lock().await;
            let temperature = config::resolve_temperature(&server.cli, &server.cfg, &session.model);
            let reasoning_effort = {
                let mutable = server.mutable.lock().await;
                mutable.reasoning_effort.clone().or_else(|| {
                    crate::config::resolve_reasoning_effort(
                        &server.cli,
                        &server.cfg,
                        &session.provider,
                        &session.model,
                    )
                })
            };
            (temperature, reasoning_effort)
        };
        let reasoning_enabled = server.mutable.lock().await.reasoning_enabled;
        let context = server.context.lock().await;
        let agent = {
            #[cfg(feature = "mcp")]
            let mut mcp_guard = {
                let guard = server.mcp_manager.lock().await;
                guard
            };
            #[cfg(feature = "mcp")]
            if mcp_guard.is_none() {
                if let Some(configs) = server.cfg.mcp_servers.as_ref() {
                    if !configs.is_empty() {
                        *mcp_guard =
                            Some(crate::extras::mcp::McpClientManager::connect_all(configs).await);
                    }
                }
            }

            build_agent(
                model,
                &server.cli,
                &server.cfg,
                &context,
                server.permission.clone(),
                server.ask_tx.clone(),
                server.sandbox.clone(),
                reasoning_enabled,
                reasoning_effort.as_deref(),
                temperature,
                #[cfg(feature = "mcp")]
                mcp_guard.as_ref(),
            )
            .await
        };
        drop(context);
        let mut runner = agent.spawn_runner(text, history);
        {
            let mut mutable = server.mutable.lock().await;
            mutable.abort_handle = Some(runner.abort_handle);
        }

        let mut response_buf = String::new();
        let mut response_start_line: Option<usize> = None;
        let mut reasoning_buf = String::new();
        let mut reasoning_start_line: Option<usize> = None;

        while let Some(event) = runner.event_rx.recv().await {
            match event {
                AgentEvent::Reasoning(text) => {
                    let safe = sanitize_output(&text);
                    if safe.is_empty() {
                        continue;
                    }
                    reasoning_buf.push_str(&safe);
                    let artifact = match server
                        .write_artifact_file(turn, "thinking", "thinking.txt", &reasoning_buf)
                        .await
                    {
                        Ok(artifact) => Some(artifact),
                        Err(e) => {
                            tracing::warn!("failed to write Emacs reasoning artifact: {e}");
                            None
                        }
                    };
                    if response_start_line.is_none() {
                        if let Some(artifact) = artifact.as_ref() {
                            let start = match reasoning_start_line {
                                Some(start) => start,
                                None => {
                                    let start = server.mutable.lock().await.line_count;
                                    reasoning_start_line = Some(start);
                                    start
                                }
                            };
                            server
                                .send_render_event(
                                    "reasoning-render",
                                    turn,
                                    start,
                                    vec![WireLine::with_artifact(
                                        format!("thinking: {}", format_bytes(artifact.bytes)),
                                        "zs-reasoning",
                                        artifact.clone(),
                                    )],
                                )
                                .await;
                        }
                    }
                    let preview = preview_text(&safe);
                    server
                        .broadcast_event(
                            "reasoning",
                            format!(
                                " :turn {} :preview {}{}",
                                turn,
                                sexp_quote(&preview),
                                artifact_field(artifact.as_ref()),
                            ),
                        )
                        .await;
                }
                AgentEvent::Token(text) => {
                    let safe = sanitize_output(&text);
                    response_buf.push_str(&safe);
                    let cols = server.mutable.lock().await.cols;
                    let lines = with_source_lines(
                        render_assistant_lines(&response_buf, cols, false),
                        assistant_index,
                        MessageRole::Assistant,
                    );
                    let start = match response_start_line {
                        Some(start) => start,
                        None => {
                            let start = server.mutable.lock().await.line_count;
                            response_start_line = Some(start);
                            start
                        }
                    };
                    server
                        .send_render_event("assistant-render", turn, start, lines)
                        .await;
                }
                AgentEvent::ToolCall { name, args } => {
                    if response_start_line.is_some() {
                        server
                            .append_lines(
                                "assistant-render",
                                turn,
                                vec![
                                    blank_line()
                                        .with_source(assistant_index, MessageRole::Assistant),
                                ],
                            )
                            .await;
                    }
                    response_buf.clear();
                    response_start_line = None;
                    // A later model call may emit a new reasoning stream after this
                    // tool result. Start that as a new rendered segment so replacing
                    // `thinking: ...` does not delete the tool rows appended below.
                    reset_reasoning_render_segment(&mut reasoning_start_line);
                    let summary = format_tool_call_summary(&name, &args);
                    {
                        let mut session = server.session.lock().await;
                        session.add_tool_call(&name, &args);
                        if !server.cli.no_session {
                            crate::session::storage::save_session(&session)?;
                        }
                    }
                    server
                        .append_lines(
                            "tool-render",
                            turn,
                            vec![WireLine::new(
                                format!("◈ {}", sanitize_output(&summary)),
                                "zs-tool",
                            )],
                        )
                        .await;
                    server
                        .broadcast_event(
                            "tool-call",
                            format!(
                                " :turn {} :name {} :summary {} :args {}",
                                turn,
                                sexp_quote(&name),
                                sexp_quote(&summary),
                                sexp_quote(&args.to_string()),
                            ),
                        )
                        .await;
                }
                AgentEvent::SubagentToolCall { name, args } => {
                    let summary = format_tool_call_summary(&name, &args);
                    {
                        let mut session = server.session.lock().await;
                        session.add_subagent_tool_call(&name, &args);
                        if !server.cli.no_session {
                            crate::session::storage::save_session(&session)?;
                        }
                    }
                    server
                        .append_lines(
                            "tool-render",
                            turn,
                            vec![WireLine::new(
                                format!("⌥ {}", sanitize_output(&summary)),
                                "zs-tool",
                            )],
                        )
                        .await;
                    server
                        .broadcast_event(
                            "subagent-tool-call",
                            format!(
                                " :turn {} :name {} :summary {} :args {}",
                                turn,
                                sexp_quote(&name),
                                sexp_quote(&summary),
                                sexp_quote(&args.to_string()),
                            ),
                        )
                        .await;
                }
                AgentEvent::ToolResult { name, output } => {
                    let safe = {
                        let mut session = server.session.lock().await;
                        let content = tool_result_artifact_content(&mut session, &name, &output);
                        if !server.cli.no_session {
                            crate::session::storage::save_session(&session)?;
                        }
                        content
                    };
                    let artifact = match server
                        .create_artifact(turn, "tool-output", &name, &safe)
                        .await
                    {
                        Ok(artifact) => Some(artifact),
                        Err(e) => {
                            tracing::warn!("failed to write Emacs tool output artifact: {e}");
                            None
                        }
                    };
                    if let Some(artifact) = artifact.as_ref() {
                        server
                            .append_lines(
                                "tool-render",
                                turn,
                                vec![WireLine::with_artifact(
                                    format!(
                                        "  output: {} ({})",
                                        sanitize_output(&name),
                                        format_bytes(artifact.bytes),
                                    ),
                                    "zs-link",
                                    artifact.clone(),
                                )],
                            )
                            .await;
                    }
                    let preview = preview_text(&safe);
                    server
                        .broadcast_event(
                            "tool-result",
                            format!(
                                " :turn {} :name {} :chars {} :preview {}{}",
                                turn,
                                sexp_quote(&name),
                                safe.chars().count(),
                                sexp_quote(&preview),
                                artifact_field(artifact.as_ref()),
                            ),
                        )
                        .await;
                }
                AgentEvent::CompletionCall { call_index, usage } => {
                    let (tokens, context_window) = {
                        let mut session = server.session.lock().await;
                        let real = usage.input_tokens.saturating_add(usage.output_tokens);
                        if real > session.total_estimated_tokens {
                            session.total_estimated_tokens = real;
                        }
                        (session.effective_context_tokens(), session.context_window)
                    };
                    server
                        .broadcast_event(
                            "completion-call",
                            format!(
                                " :turn {} :call-index {} :input-tokens {} :output-tokens {} :tokens {} :context-window {}",
                                turn, call_index, usage.input_tokens, usage.output_tokens, tokens, context_window,
                            ),
                        )
                        .await;
                }
                AgentEvent::Done {
                    response,
                    usage,
                    reasoning,
                } => {
                    let cols = server.mutable.lock().await.cols;
                    let start = match response_start_line {
                        Some(start) => start,
                        None => server.mutable.lock().await.line_count,
                    };
                    let mut lines = with_source_lines(
                        render_assistant_lines(&response, cols, false),
                        assistant_index,
                        MessageRole::Assistant,
                    );
                    let latex_items = attach_latex_metadata(&server, turn, start, &mut lines).await;
                    lines.push(blank_line().with_source(assistant_index, MessageRole::Assistant));
                    lines.push(blank_line().with_source(assistant_index, MessageRole::Assistant));
                    server
                        .send_render_event("assistant-render", turn, start, lines)
                        .await;
                    let (tokens, context_window, billable_input_tokens, billable_output_tokens) = {
                        let mut session = server.session.lock().await;
                        session.add_message_with_reasoning_and_usage(
                            MessageRole::Assistant,
                            &response,
                            reasoning,
                            Some(usage.into()),
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
                        session.set_calibration(usage.input_tokens, usage.output_tokens);
                        if !server.cli.no_session {
                            crate::session::storage::save_session(&session)?;
                        }
                        (
                            session.effective_context_tokens(),
                            session.context_window,
                            billable_input_tokens,
                            billable_output_tokens,
                        )
                    };
                    server.update_meta_from_session().await;
                    server
                        .broadcast_event(
                            "done",
                            format!(
                                " :turn {} :input-tokens {} :output-tokens {} :tokens {} :context-window {}",
                                turn, billable_input_tokens, billable_output_tokens, tokens, context_window,
                            ),
                        )
                        .await;
                    if !latex_items.is_empty() {
                        server
                            .broadcast_event(
                                "latex-preview-ready",
                                format!(
                                    " :turn {} :items {}",
                                    turn,
                                    latex_items_to_sexp(&latex_items),
                                ),
                            )
                            .await;
                    }
                    return Ok(Some(response.to_string()));
                }
                AgentEvent::Error(e) => {
                    server
                        .broadcast_event(
                            "error",
                            format!(" :turn {} :message {}", turn, sexp_quote(&e)),
                        )
                        .await;
                    return Ok(None);
                }
            }
        }

        let should_report = {
            let mutable = server.mutable.lock().await;
            should_report_agent_ended(&mutable, turn)
        };
        if should_report {
            server
                .broadcast_event(
                    "error",
                    format!(
                        " :turn {} :message {}",
                        turn,
                        sexp_quote("agent ended without response")
                    ),
                )
                .await;
        }
        Ok(None)
    }

    fn should_report_agent_ended(mutable: &MutableState, turn: u64) -> bool {
        mutable.running && mutable.turn == turn
    }

    #[cfg(feature = "loop")]
    enum NextLoop {
        None,
        Stopped {
            reason: &'static str,
        },
        Start {
            turn: u64,
            prompt: String,
            fields: String,
        },
    }

    #[cfg(feature = "loop")]
    async fn continue_loop_after_turn(
        server: Arc<Server>,
        completed_turn: u64,
        response: String,
    ) -> NextLoop {
        let current = {
            let mutable = server.mutable.lock().await;
            mutable
                .loop_state
                .as_ref()
                .filter(|ls| ls.active)
                .map(|ls| {
                    let summary: String = response
                        .chars()
                        .take(crate::extras::r#loop::SUMMARY_TRUNCATION_CHARS)
                        .collect();
                    (ls.iteration, ls.build_prompt(), ls.run_cmd.clone(), summary)
                })
        };
        let Some((iteration, prompt, run_cmd, summary)) = current else {
            return NextLoop::None;
        };

        let validation_output = run_loop_validation(run_cmd.as_deref()).await;
        if let Err(e) = crate::extras::r#loop::transcript::save_iteration(
            &server.current_session_id().await,
            iteration,
            &prompt,
            &response,
            validation_output.as_deref(),
            &summary,
        ) {
            tracing::warn!("failed to save Emacs loop transcript: {e}");
        }

        {
            let mut mutable = server.mutable.lock().await;
            if mutable.turn != completed_turn || mutable.running {
                NextLoop::None
            } else if let Some(ls) = mutable.loop_state.as_mut().filter(|ls| ls.active) {
                ls.last_summary = Some(summary);
                ls.last_run_output = validation_output;
                ls.iteration = ls.iteration.saturating_add(1);
                if ls.should_stop() {
                    ls.active = false;
                    mutable.loop_state = None;
                    NextLoop::Stopped { reason: "max" }
                } else {
                    let prompt = ls.build_prompt();
                    let fields = loop_fields(ls);
                    mutable.running = true;
                    mutable.turn = mutable.turn.saturating_add(1);
                    NextLoop::Start {
                        turn: mutable.turn,
                        prompt,
                        fields,
                    }
                }
            } else {
                NextLoop::None
            }
        }
    }

    #[cfg(feature = "loop")]
    async fn run_loop_validation(run_cmd: Option<&str>) -> Option<String> {
        let cmd = run_cmd?.trim();
        if cmd.is_empty() {
            return None;
        }
        let shell = if cfg!(windows) { "powershell" } else { "sh" };
        let shell_arg = if cfg!(windows) { "-Command" } else { "-c" };
        match ProcessCommand::new(shell)
            .arg(shell_arg)
            .arg(cmd)
            .output()
            .await
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                if stderr.is_empty() {
                    Some(stdout)
                } else if stdout.is_empty() {
                    Some(stderr)
                } else {
                    Some(format!("{}\n{}", stdout, stderr))
                }
            }
            Err(e) => Some(format!("error: {e}")),
        }
    }

    async fn permission_pump(server: Arc<Server>, mut ask_rx: AskReceiver) {
        while let Some(req) = ask_rx.recv().await {
            let tool = req.tool.to_string();
            let input = req.input.clone();
            let suggested = suggest_pattern(&tool, &input);
            let request_id = {
                let mut mutable = server.mutable.lock().await;
                let id = mutable.next_permission_id;
                mutable.next_permission_id = mutable.next_permission_id.saturating_add(1);
                mutable.pending_permissions.insert(id, req);
                id
            };
            server
                .broadcast_event(
                    "permission-request",
                    format!(
                        " :request {} :tool {} :input {} :suggested-pattern {}",
                        request_id,
                        sexp_quote(&tool),
                        sexp_quote(&input),
                        sexp_quote(&suggested),
                    ),
                )
                .await;
        }
    }

    async fn live_output_pump(server: Arc<Server>, mut rx: mpsc::Receiver<BashLiveOutputRequest>) {
        while let Some(req) = rx.recv().await {
            let turn = server.mutable.lock().await.turn;
            let artifact = match server
                .create_artifact(turn, "live-tool-output", "bash-live-output", "")
                .await
            {
                Ok(artifact) => artifact,
                Err(e) => {
                    tracing::warn!("failed to create live bash output artifact: {e}");
                    let _ = req.reply.send(None);
                    continue;
                }
            };
            let path = artifact.path.clone();
            server
                .append_lines(
                    "tool-render",
                    turn,
                    vec![WireLine::with_artifact(
                        format!("  live output: {}", sanitize_output(&req.command)),
                        "zs-link",
                        artifact,
                    )],
                )
                .await;
            let _ = req.reply.send(Some(path));
        }
    }

    fn render_session_lines(
        session: &Session,
        cli: &Cli,
        cfg: &Config,
        context: &ContextFiles,
        cols: usize,
    ) -> Vec<WireLine> {
        let mut out = Vec::new();
        if context.agents.is_some() {
            out.push(WireLine::new("[system] loaded AGENTS.md", "zs-muted"));
            out.push(blank_line());
        }
        #[cfg(feature = "archmd")]
        if context.architecture.is_some() {
            out.push(WireLine::new("[system] loaded ARCHITECTURE.md", "zs-muted"));
            out.push(blank_line());
        }
        if !session.compactions.is_empty() {
            out.push(WireLine::new(
                format!(
                    "compacted {} times (saved ~{} tokens)",
                    session.compactions.len(),
                    session
                        .compactions
                        .last()
                        .map(|c| c.token_savings)
                        .unwrap_or(0),
                ),
                "zs-muted",
            ));
            out.push(blank_line());
        }
        for (message_index, msg) in session.messages.iter().enumerate() {
            match msg.role {
                MessageRole::User => out.extend(
                    render_user_lines(&msg.content)
                        .into_iter()
                        .map(|line| line.with_source(message_index, msg.role)),
                ),
                MessageRole::Assistant => {
                    out.extend(
                        render_assistant_lines(&msg.content, cols, true)
                            .into_iter()
                            .map(|line| line.with_source(message_index, msg.role)),
                    );
                }
                MessageRole::System => {
                    for line in msg.content.lines() {
                        out.push(
                            WireLine::new(format!("# {}", line), "zs-muted")
                                .with_source(message_index, msg.role),
                        );
                    }
                    out.push(blank_line().with_source(message_index, msg.role));
                }
                MessageRole::ToolCall => {
                    out.push(
                        WireLine::new(format!("◈ {}", sanitize_output(&msg.content)), "zs-tool")
                            .with_source(message_index, msg.role),
                    );
                    out.push(blank_line().with_source(message_index, msg.role));
                }
                MessageRole::ToolResult => {
                    let output = msg
                        .content
                        .split_once(":\n")
                        .map(|(_, output)| output)
                        .unwrap_or(&msg.content);
                    for line in output.lines() {
                        out.push(
                            WireLine::new(
                                format!("◈ result {}", sanitize_output(line)),
                                "zs-muted",
                            )
                            .with_source(message_index, msg.role),
                        );
                    }
                    out.push(blank_line().with_source(message_index, msg.role));
                }
                MessageRole::SubagentToolCall => {
                    out.push(
                        WireLine::new(format!("⌥ {}", sanitize_output(&msg.content)), "zs-tool")
                            .with_source(message_index, msg.role),
                    );
                    out.push(blank_line().with_source(message_index, msg.role));
                }
            }
        }

        if session.messages.is_empty() {
            let cwd = std::env::current_dir().ok();
            let cwd_str = cwd
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or(".");
            out.push(WireLine::new(
                format!(
                    "[>] zerostack {} | {} | {}",
                    env!("CARGO_PKG_VERSION"),
                    cli.resolve_model(cfg),
                    cwd_str,
                ),
                "zs-heading",
            ));
            out.push(WireLine::new(
                "──────────────────────────────────────────────────",
                "zs-heading",
            ));
            out.push(WireLine::new(
                "Ready for Emacs; connect to this session socket",
                "zs-normal",
            ));
            out.push(blank_line());
            out.push(blank_line());
        }
        out
    }

    fn with_source_lines(
        lines: Vec<WireLine>,
        message_index: usize,
        role: MessageRole,
    ) -> Vec<WireLine> {
        lines
            .into_iter()
            .map(|line| line.with_source(message_index, role))
            .collect()
    }

    fn render_user_lines(text: &str) -> Vec<WireLine> {
        let mut out = Vec::new();
        if text.is_empty() {
            out.push(WireLine::new("> ", "zs-user"));
        } else {
            for line in text.lines() {
                out.push(WireLine::new(
                    format!("> {}", sanitize_output(line)),
                    "zs-user",
                ));
            }
        }
        out.push(blank_line());
        out
    }

    fn render_assistant_lines(text: &str, cols: usize, trailing_blank: bool) -> Vec<WireLine> {
        let mut out = markdown_to_wire_lines(text, cols);
        if out.is_empty() {
            out.push(WireLine::new("< ", "zs-normal"));
        } else if let Some(first) = out.first_mut() {
            first.text.insert_str(0, "< ");
            if first.spans.is_empty() {
                first.spans.push(WireSpan {
                    text: first.text.clone(),
                    face: first.face,
                });
            } else {
                first.spans.insert(
                    0,
                    WireSpan {
                        text: "< ".to_string(),
                        face: "zs-normal",
                    },
                );
            }
        }
        if trailing_blank {
            out.push(blank_line());
        }
        out
    }

    fn markdown_to_wire_lines(text: &str, cols: usize) -> Vec<WireLine> {
        let options = MdOptions::ENABLE_STRIKETHROUGH
            | MdOptions::ENABLE_TABLES
            | MdOptions::ENABLE_TASKLISTS;
        let parser = MdParser::new_ext(text, options);
        let mut out = Vec::new();
        let mut line = SpanLine::new("zs-normal");
        let mut faces = vec!["zs-normal"];
        let mut list_stack: Vec<Option<u64>> = Vec::new();
        let mut in_code_block = false;
        let mut in_table = false;
        let mut table_alignments: Vec<MdAlignment> = Vec::new();
        let mut table_rows: Vec<Vec<String>> = Vec::new();
        let mut table_row: Vec<String> = Vec::new();
        let mut table_cell = String::new();

        for event in parser {
            match event {
                MdEvent::Start(tag) => match tag {
                    MdTag::Paragraph => {}
                    MdTag::Heading { .. } => {
                        line.flush(&mut out, cols);
                        line.face = "zs-heading";
                        faces.push("zs-heading");
                    }
                    MdTag::CodeBlock(_) => {
                        line.flush(&mut out, cols);
                        line.face = "zs-code-block";
                        faces.push("zs-code-block");
                        in_code_block = true;
                    }
                    MdTag::BlockQuote(_) => {
                        line.flush(&mut out, cols);
                        line.face = "zs-quote";
                        faces.push("zs-quote");
                        line.push("│ ", "zs-quote");
                    }
                    MdTag::List(start) => list_stack.push(start),
                    MdTag::Item => {
                        line.flush(&mut out, cols);
                        let marker = match list_stack.last_mut().and_then(Option::as_mut) {
                            Some(n) => {
                                let current = *n;
                                *n = n.saturating_add(1);
                                format!("{current}. ")
                            }
                            None => "• ".to_string(),
                        };
                        line.push(&marker, "zs-list-marker");
                    }
                    MdTag::Emphasis => faces.push("zs-italic"),
                    MdTag::Strong => faces.push("zs-bold"),
                    MdTag::Link { .. } => faces.push("zs-link"),
                    MdTag::Table(alignments) => {
                        line.flush(&mut out, cols);
                        in_table = true;
                        table_alignments = alignments;
                        table_rows.clear();
                    }
                    MdTag::TableHead | MdTag::TableRow if in_table => {
                        table_row.clear();
                    }
                    MdTag::TableCell if in_table => {
                        table_cell.clear();
                    }
                    MdTag::TableHead | MdTag::TableRow | MdTag::TableCell => {}
                    _ => {}
                },
                MdEvent::End(tag) => match tag {
                    MdTagEnd::Paragraph => line.flush(&mut out, cols),
                    MdTagEnd::Heading(_) => {
                        line.flush(&mut out, cols);
                        out.push(blank_line());
                        pop_face(&mut faces, "zs-heading");
                        line.face = *faces.last().unwrap_or(&"zs-normal");
                    }
                    MdTagEnd::CodeBlock => {
                        line.flush(&mut out, cols);
                        out.push(blank_line());
                        in_code_block = false;
                        pop_face(&mut faces, "zs-code-block");
                        line.face = *faces.last().unwrap_or(&"zs-normal");
                    }
                    MdTagEnd::BlockQuote(_) => {
                        line.flush(&mut out, cols);
                        pop_face(&mut faces, "zs-quote");
                        line.face = *faces.last().unwrap_or(&"zs-normal");
                    }
                    MdTagEnd::List(_) => {
                        list_stack.pop();
                        line.flush(&mut out, cols);
                    }
                    MdTagEnd::Item => line.flush(&mut out, cols),
                    MdTagEnd::Emphasis => pop_face(&mut faces, "zs-italic"),
                    MdTagEnd::Strong => pop_face(&mut faces, "zs-bold"),
                    MdTagEnd::Link => pop_face(&mut faces, "zs-link"),
                    MdTagEnd::Table => {
                        flush_wire_table(&table_rows, &table_alignments, cols, &mut out);
                        in_table = false;
                        table_alignments.clear();
                        table_rows.clear();
                    }
                    MdTagEnd::TableHead | MdTagEnd::TableRow if in_table => {
                        if !table_row.is_empty() {
                            table_rows.push(std::mem::take(&mut table_row));
                        }
                    }
                    MdTagEnd::TableCell if in_table => {
                        table_row.push(table_cell.trim().to_string());
                        table_cell.clear();
                    }
                    MdTagEnd::TableHead | MdTagEnd::TableRow | MdTagEnd::TableCell => {
                        line.flush(&mut out, cols)
                    }
                    _ => {}
                },
                MdEvent::Text(value) => {
                    if in_table {
                        if !table_cell.is_empty() {
                            table_cell.push(' ');
                        }
                        table_cell.push_str(&value);
                    } else {
                        line.push(&value, *faces.last().unwrap_or(&"zs-normal"));
                    }
                }
                MdEvent::Code(value) => {
                    if in_table {
                        if !table_cell.is_empty() {
                            table_cell.push(' ');
                        }
                        table_cell.push('`');
                        table_cell.push_str(&value);
                        table_cell.push('`');
                    } else {
                        line.push(&value, "zs-code");
                    }
                }
                MdEvent::SoftBreak | MdEvent::HardBreak => {
                    if in_table {
                        table_cell.push(' ');
                    } else if in_code_block {
                        line.flush(&mut out, cols);
                    } else {
                        line.push(" ", *faces.last().unwrap_or(&"zs-normal"));
                    }
                }
                MdEvent::TaskListMarker(checked) => {
                    line.push(if checked { "[x] " } else { "[ ] " }, "zs-list-marker");
                }
                _ => {}
            }
        }
        line.flush(&mut out, cols);
        out
    }

    fn flush_wire_table(
        rows: &[Vec<String>],
        alignments: &[MdAlignment],
        cols: usize,
        out: &mut Vec<WireLine>,
    ) {
        if rows.is_empty() {
            return;
        }

        let col_count = rows.iter().map(|row| row.len()).max().unwrap_or(0);
        if col_count == 0 {
            return;
        }

        let mut col_widths = vec![0usize; col_count];
        for row in rows {
            for (idx, cell) in row.iter().enumerate() {
                col_widths[idx] = col_widths[idx].max(display_width(cell));
            }
        }

        let overhead = 3 * col_count + 1;
        let available = cols.max(20).saturating_sub(overhead);
        if available == 0 {
            return;
        }

        let mut total_req: usize = col_widths.iter().sum();
        const MIN_COL_WIDTH: usize = 4;
        while total_req > available {
            let Some((widest_idx, widest_w)) = col_widths
                .iter()
                .copied()
                .enumerate()
                .max_by_key(|(_, width)| *width)
            else {
                break;
            };
            if widest_w <= MIN_COL_WIDTH {
                break;
            }
            col_widths[widest_idx] -= 1;
            total_req -= 1;
        }

        push_wire_table_rule(&col_widths, '┌', '┬', '┐', out);
        for (idx, row) in rows.iter().enumerate() {
            for table_line in format_wire_table_row(row, &col_widths, alignments) {
                out.push(WireLine::with_spans(
                    vec![WireSpan {
                        text: table_line,
                        face: "zs-table",
                    }],
                    "zs-table",
                ));
            }
            if idx == 0 && rows.len() > 1 {
                push_wire_table_rule(&col_widths, '├', '┼', '┤', out);
            }
        }
        push_wire_table_rule(&col_widths, '└', '┴', '┘', out);
    }

    fn push_wire_table_rule(
        widths: &[usize],
        left: char,
        mid: char,
        right: char,
        out: &mut Vec<WireLine>,
    ) {
        let mut text = String::new();
        text.push(left);
        for (idx, width) in widths.iter().enumerate() {
            for _ in 0..*width + 2 {
                text.push('─');
            }
            if idx + 1 < widths.len() {
                text.push(mid);
            }
        }
        text.push(right);
        out.push(WireLine::with_spans(
            vec![WireSpan {
                text,
                face: "zs-table-border",
            }],
            "zs-table-border",
        ));
    }

    fn format_wire_table_row(
        cells: &[String],
        widths: &[usize],
        alignments: &[MdAlignment],
    ) -> Vec<String> {
        let mut wrapped_cells = Vec::new();
        let mut max_subrows = 0usize;

        for (idx, cell) in cells.iter().enumerate() {
            let width = widths.get(idx).copied().unwrap_or(10);
            let wrapped: Vec<String> = if display_width(cell) <= width {
                vec![cell.clone()]
            } else {
                word_wrap(cell, width)
                    .into_iter()
                    .map(|chunk| chunk.to_string())
                    .collect()
            };
            max_subrows = max_subrows.max(wrapped.len());
            wrapped_cells.push(wrapped);
        }

        for _ in cells.len()..widths.len() {
            wrapped_cells.push(vec![String::new()]);
            max_subrows = max_subrows.max(1);
        }

        let mut lines = Vec::new();
        for subrow in 0..max_subrows {
            let mut line = String::new();
            line.push('│');
            for (idx, wrapped) in wrapped_cells.iter().enumerate() {
                let width = widths.get(idx).copied().unwrap_or(10);
                let text = wrapped.get(subrow).map(String::as_str).unwrap_or("");
                let padding = width.saturating_sub(display_width(text));
                line.push(' ');
                match alignments.get(idx).copied().unwrap_or(MdAlignment::None) {
                    MdAlignment::Center => {
                        let left_pad = padding / 2;
                        let right_pad = padding - left_pad;
                        push_spaces(&mut line, left_pad);
                        line.push_str(text);
                        push_spaces(&mut line, right_pad);
                    }
                    MdAlignment::Right => {
                        push_spaces(&mut line, padding);
                        line.push_str(text);
                    }
                    MdAlignment::None | MdAlignment::Left => {
                        line.push_str(text);
                        push_spaces(&mut line, padding);
                    }
                }
                line.push(' ');
                if idx + 1 < wrapped_cells.len() {
                    line.push('│');
                }
            }
            line.push('│');
            lines.push(line);
        }
        lines
    }

    fn push_spaces(line: &mut String, count: usize) {
        for _ in 0..count {
            line.push(' ');
        }
    }

    struct SpanLine {
        face: &'static str,
        spans: Vec<WireSpan>,
    }

    impl SpanLine {
        fn new(face: &'static str) -> Self {
            Self {
                face,
                spans: Vec::new(),
            }
        }

        fn push(&mut self, text: &str, face: &'static str) {
            if text.is_empty() {
                return;
            }
            if let Some(last) = self.spans.last_mut()
                && last.face == face
            {
                last.text.push_str(text);
                return;
            }
            self.spans.push(WireSpan {
                text: text.to_string(),
                face,
            });
        }

        fn flush(&mut self, out: &mut Vec<WireLine>, cols: usize) {
            if self.spans.is_empty() {
                return;
            }
            out.extend(wrap_spans(std::mem::take(&mut self.spans), self.face, cols));
        }
    }

    fn wrap_spans(spans: Vec<WireSpan>, line_face: &'static str, cols: usize) -> Vec<WireLine> {
        let max = cols.max(20);
        let mut out = Vec::new();
        let mut current = Vec::new();
        let mut width = 0usize;
        for span in spans {
            for part in span.text.split_inclusive('\n') {
                let (text, newline) = part
                    .strip_suffix('\n')
                    .map(|s| (s, true))
                    .unwrap_or((part, false));
                for word in text.split_inclusive(' ') {
                    let word_width = word.chars().count();
                    if width > 0 && width + word_width > max {
                        push_wrapped_line(&mut out, &mut current, line_face);
                        width = 0;
                    }
                    if !word.is_empty() {
                        current.push(WireSpan {
                            text: word.to_string(),
                            face: span.face,
                        });
                        width += word_width;
                    }
                }
                if newline {
                    push_wrapped_line(&mut out, &mut current, line_face);
                    width = 0;
                }
            }
        }
        push_wrapped_line(&mut out, &mut current, line_face);
        out
    }

    fn push_wrapped_line(
        out: &mut Vec<WireLine>,
        current: &mut Vec<WireSpan>,
        line_face: &'static str,
    ) {
        if current.is_empty() {
            return;
        }
        let spans = std::mem::take(current);
        out.push(WireLine::with_spans(spans, line_face));
    }

    fn pop_face(faces: &mut Vec<&'static str>, face: &'static str) {
        if let Some(pos) = faces.iter().rposition(|item| *item == face) {
            faces.remove(pos);
        }
    }

    async fn attach_latex_metadata(
        server: &Arc<Server>,
        turn: u64,
        replace_from: usize,
        lines: &mut [WireLine],
    ) -> Vec<LatexInfo> {
        let spans = find_latex_spans(lines);
        let mut latex_items = Vec::new();

        for (idx, span) in spans.into_iter().enumerate() {
            let id = format!("turn-{turn}-latex-{}", idx + 1);
            let document = latex_document(&span.source, span.display);
            let filename = format!("latex-{:04}.tex", idx + 1);
            let artifact = match server
                .write_artifact_file_with_mime(
                    turn,
                    "latex-source",
                    &filename,
                    &document,
                    "text/x-tex; charset=utf-8",
                )
                .await
            {
                Ok(artifact) => artifact,
                Err(e) => {
                    tracing::warn!("failed to write Emacs LaTeX artifact: {e}");
                    continue;
                }
            };
            let svg_artifact = render_latex_svg_artifact(server, turn, &filename, idx + 1).await;
            let latex = LatexInfo {
                id,
                source: span.source,
                display: span.display,
                artifact,
                svg_artifact,
                line_start: replace_from.saturating_add(span.line_start),
                col_start: span.col_start,
                line_end: replace_from.saturating_add(span.line_end),
                col_end: span.col_end,
            };
            if let Some(line) = lines.get_mut(span.line_start) {
                line.push_latex(latex.clone());
            }
            latex_items.push(latex);
        }

        latex_items
    }

    async fn render_latex_svg_artifact(
        server: &Arc<Server>,
        turn: u64,
        tex_filename: &str,
        index: usize,
    ) -> Option<ArtifactInfo> {
        match render_latex_svg_artifact_inner(server, turn, tex_filename, index).await {
            Ok(artifact) => Some(artifact),
            Err(e) => {
                tracing::warn!("failed to render Emacs LaTeX SVG artifact: {e}");
                None
            }
        }
    }

    async fn render_latex_svg_artifact_inner(
        server: &Arc<Server>,
        turn: u64,
        tex_filename: &str,
        index: usize,
    ) -> anyhow::Result<ArtifactInfo> {
        let dir = server
            .registry_dir
            .join("artifacts")
            .join(format!("turn-{turn}"));
        let stem = Path::new(tex_filename)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("LaTeX artifact filename has no stem")?;
        let dvi_filename = format!("{stem}.dvi");
        let svg_filename = format!("latex-{index:04}.svg");
        let svg_path = dir.join(&svg_filename);

        run_latex_command(&dir, tex_filename).await?;
        run_dvisvgm_command(&dir, &dvi_filename, &svg_filename).await?;

        let svg = tokio::fs::read(&svg_path)
            .await
            .with_context(|| format!("read SVG artifact {}", svg_path.display()))?;
        let preview = preview_text(&String::from_utf8_lossy(&svg));
        Ok(ArtifactInfo {
            kind: "latex-svg",
            path: svg_path,
            mime: "image/svg+xml",
            bytes: svg.len(),
            preview,
        })
    }

    async fn run_latex_command(dir: &Path, tex_filename: &str) -> anyhow::Result<()> {
        let command = std::env::var("ZEROSTACK_LATEX").unwrap_or_else(|_| "latex".to_string());
        let output = timeout(
            Duration::from_secs(8),
            ProcessCommand::new(command)
                .current_dir(dir)
                .args(["-interaction=nonstopmode", "-halt-on-error", tex_filename])
                .output(),
        )
        .await
        .context("LaTeX timed out")?
        .context("run latex")?;
        if !output.status.success() {
            anyhow::bail!(
                "latex exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    async fn run_dvisvgm_command(
        dir: &Path,
        dvi_filename: &str,
        svg_filename: &str,
    ) -> anyhow::Result<()> {
        let command = std::env::var("ZEROSTACK_DVISVGM").unwrap_or_else(|_| "dvisvgm".to_string());
        let output_arg = format!("--output={svg_filename}");
        let output = timeout(
            Duration::from_secs(8),
            ProcessCommand::new(command)
                .current_dir(dir)
                .args([
                    "--no-fonts",
                    "--exact",
                    "--page=1",
                    output_arg.as_str(),
                    dvi_filename,
                ])
                .output(),
        )
        .await
        .context("dvisvgm timed out")?
        .context("run dvisvgm")?;
        if !output.status.success() {
            anyhow::bail!(
                "dvisvgm exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    fn find_latex_spans(lines: &[WireLine]) -> Vec<LocatedLatexSpan> {
        let mut spans = Vec::new();
        let mut display_start: Option<(usize, usize, String)> = None;

        for (line_idx, line) in lines.iter().enumerate() {
            if line.face == "zs-code" {
                continue;
            }
            let text = line.text.as_str();
            let mut pos = 0;

            if let Some((start_line, start_col, mut source)) = display_start.take() {
                if let Some(end) = find_unescaped(text, "$$", pos) {
                    if !source.is_empty() {
                        source.push('\n');
                    }
                    source.push_str(&text[..end]);
                    push_latex_span(
                        &mut spans,
                        source,
                        true,
                        start_line,
                        start_col,
                        line_idx,
                        byte_to_char_idx(text, end + 2),
                    );
                    pos = end + 2;
                } else {
                    if !source.is_empty() {
                        source.push('\n');
                    }
                    source.push_str(text);
                    display_start = Some((start_line, start_col, source));
                    continue;
                }
            }

            while pos < text.len() {
                let next_display = find_unescaped(text, "$$", pos);
                let next_inline = find_inline_dollar(text, pos);

                match (next_display, next_inline) {
                    (Some(display), Some(inline)) if inline < display => {
                        if let Some(end) = find_inline_dollar(text, inline + 1) {
                            push_latex_span(
                                &mut spans,
                                text[inline + 1..end].to_string(),
                                false,
                                line_idx,
                                byte_to_char_idx(text, inline),
                                line_idx,
                                byte_to_char_idx(text, end + 1),
                            );
                            pos = end + 1;
                        } else {
                            break;
                        }
                    }
                    (Some(display), _) => {
                        let source_start = display + 2;
                        if let Some(end) = find_unescaped(text, "$$", source_start) {
                            push_latex_span(
                                &mut spans,
                                text[source_start..end].to_string(),
                                true,
                                line_idx,
                                byte_to_char_idx(text, display),
                                line_idx,
                                byte_to_char_idx(text, end + 2),
                            );
                            pos = end + 2;
                        } else {
                            display_start = Some((
                                line_idx,
                                byte_to_char_idx(text, display),
                                text[source_start..].to_string(),
                            ));
                            break;
                        }
                    }
                    (None, Some(inline)) => {
                        if let Some(end) = find_inline_dollar(text, inline + 1) {
                            push_latex_span(
                                &mut spans,
                                text[inline + 1..end].to_string(),
                                false,
                                line_idx,
                                byte_to_char_idx(text, inline),
                                line_idx,
                                byte_to_char_idx(text, end + 1),
                            );
                            pos = end + 1;
                        } else {
                            break;
                        }
                    }
                    (None, None) => break,
                }
            }
        }

        spans
    }

    #[allow(clippy::too_many_arguments)]
    fn push_latex_span(
        spans: &mut Vec<LocatedLatexSpan>,
        source: String,
        display: bool,
        line_start: usize,
        col_start: usize,
        line_end: usize,
        col_end: usize,
    ) {
        let source = source.trim().to_string();
        if source.is_empty() {
            return;
        }
        spans.push(LocatedLatexSpan {
            source,
            display,
            line_start,
            col_start,
            line_end,
            col_end,
        });
    }

    fn find_inline_dollar(text: &str, start: usize) -> Option<usize> {
        let mut pos = start;
        while let Some(found) = find_unescaped(text, "$", pos) {
            let next = found + 1;
            let is_double_before = found > 0 && text[..found].ends_with('$');
            let is_double_after = text[next..].starts_with('$');
            if !is_double_before && !is_double_after {
                return Some(found);
            }
            pos = next;
        }
        None
    }

    fn find_unescaped(text: &str, needle: &str, start: usize) -> Option<usize> {
        let mut search_from = start;
        while search_from <= text.len() {
            let rel = text[search_from..].find(needle)?;
            let idx = search_from + rel;
            if !is_escaped(text, idx) {
                return Some(idx);
            }
            search_from = idx + needle.len();
        }
        None
    }

    fn is_escaped(text: &str, byte_idx: usize) -> bool {
        let mut slash_count = 0;
        for ch in text[..byte_idx].chars().rev() {
            if ch == '\\' {
                slash_count += 1;
            } else {
                break;
            }
        }
        slash_count % 2 == 1
    }

    fn byte_to_char_idx(text: &str, byte_idx: usize) -> usize {
        text[..byte_idx].chars().count()
    }

    fn latex_document(source: &str, display: bool) -> String {
        let body = if display {
            format!("\\[\n{}\n\\]", source.trim())
        } else {
            format!("\\({}\\)", source.trim())
        };
        format!(
            "\\documentclass{{article}}\n\\usepackage{{amsmath,amssymb}}\n\\usepackage[active,tightpage,displaymath,textmath]{{preview}}\n\\pagestyle{{empty}}\n\\begin{{document}}\n\\begin{{preview}}\n{}\n\\end{{preview}}\n\\end{{document}}\n",
            body,
        )
    }

    fn blank_line() -> WireLine {
        WireLine::new(String::new(), "zs-normal")
    }

    fn reset_reasoning_render_segment(reasoning_start_line: &mut Option<usize>) {
        *reasoning_start_line = None;
    }

    fn runtime_root() -> PathBuf {
        if let Some(dir) = std::env::var_os("ZS_RUNTIME_DIR") {
            return PathBuf::from(dir);
        }
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            return PathBuf::from(dir).join("zerostack");
        }
        crate::session::storage::data_dir().join("runtime")
    }

    fn sessions_root() -> PathBuf {
        runtime_root().join("sessions")
    }

    fn ensure_private_dir(path: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(path)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn write_meta_atomic(dir: &Path, meta: &SessionMeta) -> anyhow::Result<()> {
        let temp = dir.join(format!("meta.json.tmp.{}", std::process::id()));
        let path = dir.join("meta.json");
        std::fs::write(&temp, serde_json::to_string_pretty(meta)?)?;
        std::fs::rename(temp, path)?;
        Ok(())
    }

    fn list_registered_sessions() -> anyhow::Result<Vec<SessionMeta>> {
        list_sessions_in(&sessions_root(), true)
    }

    fn list_sessions_in(root: &Path, cleanup: bool) -> anyhow::Result<Vec<SessionMeta>> {
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let socket_path = path.join("sock");
            if !socket_alive(&socket_path) {
                if cleanup {
                    let _ = std::fs::remove_dir_all(&path);
                }
                continue;
            }
            let Ok(json) = std::fs::read_to_string(path.join("meta.json")) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<SessionMeta>(&json) else {
                continue;
            };
            sessions.push(meta);
        }
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    fn socket_alive(path: &Path) -> bool {
        if !path.exists() {
            return false;
        }
        std::os::unix::net::UnixStream::connect(path).is_ok()
    }

    fn short_id(id: &str) -> &str {
        id.get(..8).unwrap_or(id)
    }

    fn thinking_label(enabled: bool) -> &'static str {
        if enabled { "on" } else { "off" }
    }

    fn apply_reasoning_effort_meta(meta: &mut SessionMeta, cfg: &Config, mutable: &MutableState) {
        meta.reasoning_effort_supported =
            crate::provider::supports_reasoning_effort(&meta.provider, &meta.model);
        meta.reasoning_effort = if meta.reasoning_effort_supported {
            mutable
                .reasoning_effort
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| {
                    crate::config::resolve_reasoning_effort(
                        &Cli::default(),
                        cfg,
                        &meta.provider,
                        &meta.model,
                    )
                    .map(|s| s.to_string())
                })
        } else {
            None
        };
    }

    async fn current_provider_model(server: &Arc<Server>) -> (String, String) {
        let session = server.session.lock().await;
        (session.provider.to_string(), session.model.to_string())
    }

    async fn clear_unsupported_reasoning_effort(server: &Arc<Server>) {
        let (provider, model) = current_provider_model(server).await;
        if !crate::provider::supports_reasoning_effort(&provider, &model) {
            server.mutable.lock().await.reasoning_effort = None;
        }
    }

    fn meta_to_sexp(meta: &SessionMeta) -> String {
        format!(
            "(:session {} :pid {} :cwd {} :model {} :provider {} :created-at {} :updated-at {} :title {} :tokens {} :context-window {} :protocol {} :socket {} :thinking {} :reasoning-effort-supported {} :reasoning-effort {})",
            sexp_quote(&meta.session_id),
            meta.pid,
            sexp_quote(&meta.cwd),
            sexp_quote(&meta.model),
            sexp_quote(&meta.provider),
            sexp_quote(&meta.created_at),
            sexp_quote(&meta.updated_at),
            sexp_quote(&meta.title),
            meta.tokens,
            meta.context_window,
            meta.protocol,
            sexp_quote(&meta.socket),
            sexp_quote(&meta.thinking),
            if meta.reasoning_effort_supported {
                "t"
            } else {
                "nil"
            },
            meta.reasoning_effort
                .as_deref()
                .map(sexp_quote)
                .unwrap_or_else(|| "nil".to_string()),
        )
    }

    fn parse_command(input: &str) -> anyhow::Result<Command> {
        let sexp = parse_sexp(input)?;
        let Sexp::List(items) = sexp else {
            anyhow::bail!("command must be a list");
        };
        let Some(Sexp::Atom(name)) = items.first() else {
            anyhow::bail!("command list must start with an atom");
        };
        let mut args = HashMap::new();
        let mut idx = 1;
        while idx < items.len() {
            let Sexp::Atom(key) = &items[idx] else {
                anyhow::bail!("expected keyword at argument {}", idx);
            };
            if !key.starts_with(':') {
                anyhow::bail!("expected keyword, got '{}'", key);
            }
            let Some(value) = items.get(idx + 1) else {
                anyhow::bail!("missing value for {}", key);
            };
            args.insert(key.trim_start_matches(':').to_string(), value.clone());
            idx += 2;
        }
        Ok(Command {
            name: name.clone(),
            args,
        })
    }

    fn parse_sexp(input: &str) -> anyhow::Result<Sexp> {
        let mut parser = Parser::new(input);
        let value = parser.parse_expr()?;
        parser.skip_ws();
        if parser.peek().is_some() {
            anyhow::bail!("trailing data after S-expression");
        }
        Ok(value)
    }

    struct Parser<'a> {
        input: &'a str,
        pos: usize,
    }

    impl<'a> Parser<'a> {
        fn new(input: &'a str) -> Self {
            Self { input, pos: 0 }
        }

        fn parse_expr(&mut self) -> anyhow::Result<Sexp> {
            self.skip_ws();
            match self.peek() {
                Some('(') => self.parse_list(),
                Some('"') => self.parse_string().map(Sexp::Str),
                Some(')') => anyhow::bail!("unexpected ')'"),
                Some(_) => self.parse_atom().map(Sexp::Atom),
                None => anyhow::bail!("unexpected end of input"),
            }
        }

        fn parse_list(&mut self) -> anyhow::Result<Sexp> {
            self.bump();
            let mut items = Vec::new();
            loop {
                self.skip_ws();
                match self.peek() {
                    Some(')') => {
                        self.bump();
                        return Ok(Sexp::List(items));
                    }
                    Some(_) => items.push(self.parse_expr()?),
                    None => anyhow::bail!("unterminated list"),
                }
            }
        }

        fn parse_string(&mut self) -> anyhow::Result<String> {
            self.bump();
            let mut out = String::new();
            while let Some(ch) = self.bump() {
                match ch {
                    '"' => return Ok(out),
                    '\\' => {
                        let escaped = self.bump().context("unterminated string escape")?;
                        match escaped {
                            'n' => out.push('\n'),
                            'r' => out.push('\r'),
                            't' => out.push('\t'),
                            '"' => out.push('"'),
                            '\\' => out.push('\\'),
                            other => out.push(other),
                        }
                    }
                    other => out.push(other),
                }
            }
            anyhow::bail!("unterminated string")
        }

        fn parse_atom(&mut self) -> anyhow::Result<String> {
            let start = self.pos;
            while let Some(ch) = self.peek() {
                if ch.is_whitespace() || matches!(ch, '(' | ')' | '"') {
                    break;
                }
                self.bump();
            }
            if self.pos == start {
                anyhow::bail!("expected atom");
            }
            Ok(self.input[start..self.pos].to_string())
        }

        fn skip_ws(&mut self) {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }
        }

        fn peek(&self) -> Option<char> {
            self.input[self.pos..].chars().next()
        }

        fn bump(&mut self) -> Option<char> {
            let ch = self.peek()?;
            self.pos += ch.len_utf8();
            Some(ch)
        }
    }

    fn request_arg(cmd: &Command) -> Option<String> {
        cmd.args.get("request").map(sexp_value_to_wire)
    }

    fn string_arg(cmd: &Command, key: &str) -> Option<String> {
        match cmd.args.get(key)? {
            Sexp::Str(s) => Some(s.clone()),
            Sexp::Atom(s) if s != "nil" => Some(s.clone()),
            _ => None,
        }
    }

    fn atom_arg(cmd: &Command, key: &str) -> Option<String> {
        match cmd.args.get(key)? {
            Sexp::Atom(s) if s != "nil" => Some(s.clone()),
            Sexp::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn usize_arg(cmd: &Command, key: &str) -> Option<usize> {
        atom_arg(cmd, key)?.parse().ok()
    }

    fn u64_arg(cmd: &Command, key: &str) -> Option<u64> {
        atom_arg(cmd, key)?.parse().ok()
    }

    fn sexp_value_to_wire(value: &Sexp) -> String {
        match value {
            Sexp::Atom(atom) => atom.clone(),
            Sexp::Str(s) => sexp_quote(s),
            Sexp::List(items) => format!(
                "({})",
                items
                    .iter()
                    .map(sexp_value_to_wire)
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        }
    }

    async fn send_ok(out: &mpsc::Sender<String>, request: Option<String>, fields: String) {
        let request = request.unwrap_or_else(|| "nil".to_string());
        let _ = out
            .send(format!("(ok :request {}{})", request, fields))
            .await;
    }

    async fn send_error(out: &mpsc::Sender<String>, request: Option<String>, message: &str) {
        let request = request.unwrap_or_else(|| "nil".to_string());
        let _ = out
            .send(format!(
                "(error :request {} :message {})",
                request,
                sexp_quote(message),
            ))
            .await;
    }

    fn bool_atom(value: bool) -> &'static str {
        if value { "t" } else { "nil" }
    }

    #[cfg(feature = "loop")]
    fn loop_fields(loop_state: &crate::extras::r#loop::LoopState) -> String {
        format!(
            " :active {} :iteration {} :label {} :max {} :plan {} :prompt {}",
            bool_atom(loop_state.active),
            loop_state.iteration,
            sexp_quote(&loop_state.iteration_label()),
            loop_state
                .max_iterations
                .map(|max| max.to_string())
                .unwrap_or_else(|| "nil".to_string()),
            sexp_quote(loop_state.plan_file.to_string_lossy().as_ref()),
            sexp_quote(&loop_state.prompt),
        )
    }

    fn artifact_to_sexp(artifact: &ArtifactInfo) -> String {
        format!(
            "(:kind {} :path {} :mime {} :bytes {} :preview {} :ephemeral t :expires process-exit)",
            artifact.kind,
            sexp_quote(artifact.path.to_string_lossy().as_ref()),
            sexp_quote(artifact.mime),
            artifact.bytes,
            sexp_quote(&artifact.preview),
        )
    }

    fn artifact_field(artifact: Option<&ArtifactInfo>) -> String {
        match artifact {
            Some(artifact) => format!(" :artifact {}", artifact_to_sexp(artifact)),
            None => " :artifact nil".to_string(),
        }
    }

    fn latex_to_sexp(latex: &LatexInfo) -> String {
        let svg_artifact = latex
            .svg_artifact
            .as_ref()
            .map(|artifact| artifact_to_sexp(artifact))
            .unwrap_or_else(|| "nil".to_string());
        format!(
            "(:id {} :display {} :source {} :line-start {} :col-start {} :line-end {} :col-end {} :artifact {} :svg-artifact {})",
            sexp_quote(&latex.id),
            bool_atom(latex.display),
            sexp_quote(&latex.source),
            latex.line_start,
            latex.col_start,
            latex.line_end,
            latex.col_end,
            artifact_to_sexp(&latex.artifact),
            svg_artifact,
        )
    }

    fn latex_items_to_sexp(items: &[LatexInfo]) -> String {
        format!(
            "({})",
            items
                .iter()
                .map(latex_to_sexp)
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    fn attachment_items_to_sexp(items: &[AttachmentItem]) -> String {
        format!(
            "({})",
            items
                .iter()
                .map(|item| {
                    format!(
                        "(:index {} :kind {} :path {} :bytes {}{})",
                        item.index,
                        item.kind,
                        sexp_quote(item.path.to_string_lossy().as_ref()),
                        item.bytes,
                        item.mime
                            .as_deref()
                            .map(|mime| format!(" :mime {}", sexp_quote(mime)))
                            .unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    fn lines_to_sexp(lines: &[WireLine]) -> String {
        format!(
            "({})",
            lines
                .iter()
                .map(|line| {
                    let artifact = line
                        .artifact
                        .as_ref()
                        .map(|artifact| format!(" :artifact {}", artifact_to_sexp(artifact)))
                        .unwrap_or_default();
                    let latex = if line.latex.is_empty() {
                        String::new()
                    } else {
                        format!(" :latex {}", latex_items_to_sexp(&line.latex))
                    };
                    let spans = if line.spans.is_empty() {
                        String::new()
                    } else {
                        format!(" :spans {}", spans_to_sexp(&line.spans))
                    };
                    let source = match (line.message_index, line.role) {
                        (Some(index), Some(role)) => {
                            format!(" :message-index {} :role {}", index, role_atom(role))
                        }
                        _ => String::new(),
                    };
                    format!(
                        "(:text {} :face {}{}{}{}{})",
                        sexp_quote(&line.text),
                        line.face,
                        spans,
                        artifact,
                        latex,
                        source,
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    fn spans_to_sexp(spans: &[WireSpan]) -> String {
        format!(
            "({})",
            spans
                .iter()
                .map(|span| format!("(:text {} :face {})", sexp_quote(&span.text), span.face))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    fn preview_text(input: &str) -> String {
        let mut out = String::new();
        let mut last_space = false;
        let mut count = 0;

        for ch in input.chars() {
            if count >= ARTIFACT_PREVIEW_CHARS {
                out.push_str("...");
                break;
            }

            let ch = match ch {
                '\n' | '\r' | '\t' => ' ',
                ch if ch.is_control() => continue,
                ch => ch,
            };

            if ch.is_whitespace() {
                if !last_space && !out.is_empty() {
                    out.push(' ');
                    count += 1;
                }
                last_space = true;
            } else {
                out.push(ch);
                count += 1;
                last_space = false;
            }
        }

        out.trim_end().to_string()
    }

    fn safe_filename(input: &str) -> String {
        let mut out = String::new();
        let mut last_dash = false;

        for ch in input.chars().take(64) {
            let ch = if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else if matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            };
            if ch == '-' {
                if last_dash {
                    continue;
                }
                last_dash = true;
            } else {
                last_dash = false;
            }
            out.push(ch);
        }

        let trimmed = out.trim_matches(|ch| matches!(ch, '-' | '.')).to_string();
        if trimmed.is_empty() {
            "artifact".to_string()
        } else {
            trimmed
        }
    }

    fn format_bytes(bytes: usize) -> String {
        const KB: f64 = 1024.0;
        const MB: f64 = 1024.0 * 1024.0;
        if bytes < 1024 {
            format!("{} B", bytes)
        } else if bytes < 1024 * 1024 {
            format!("{:.1} KB", bytes as f64 / KB)
        } else {
            format!("{:.1} MB", bytes as f64 / MB)
        }
    }

    fn sexp_quote(input: &str) -> String {
        let mut out = String::with_capacity(input.len() + 2);
        out.push('"');
        for ch in input.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                ch if ch.is_control() => out.push(' '),
                ch => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn quote_escapes_strings_for_one_line_read() {
            assert_eq!(sexp_quote("a \"b\"\\c\n"), "\"a \\\"b\\\"\\\\c\\n\"");
        }

        #[test]
        fn parse_command_reads_keywords_and_strings() {
            let cmd =
                parse_command("(prompt :request 7 :text \"hello\\nworld\" :cols 80)").unwrap();
            assert_eq!(cmd.name, "prompt");
            assert_eq!(request_arg(&cmd).as_deref(), Some("7"));
            assert_eq!(string_arg(&cmd, "text").as_deref(), Some("hello\nworld"));
            assert_eq!(usize_arg(&cmd, "cols"), Some(80));
        }

        #[test]
        fn compact_command_reads_instructions_and_serializes_outcome() {
            let cmd = parse_command(
                "(compact :request 12 :instructions \"preserve test failure details\")",
            )
            .unwrap();
            assert_eq!(cmd.name, "compact");
            assert_eq!(request_arg(&cmd).as_deref(), Some("12"));
            assert_eq!(
                string_arg(&cmd, "instructions").as_deref(),
                Some("preserve test failure details"),
            );

            let fork = parse_command("(fork :request 13 :index 2)").unwrap();
            assert_eq!(fork.name, "fork");
            assert_eq!(request_arg(&fork).as_deref(), Some("13"));
            assert_eq!(usize_arg(&fork, "index"), Some(2));

            let outcome = CompactionOutcome {
                compacted: true,
                messages: 3,
                saved_tokens: 1200,
                message: "compressed \"old\" context".to_string(),
            };
            assert_eq!(
                compact_outcome_fields(&outcome),
                " :compacted t :messages 3 :saved-tokens 1200 :message \"compressed \\\"old\\\" context\"",
            );
        }

        #[test]
        fn file_commands_read_paths_and_serialize_items() {
            let cmd = parse_command("(file-add :request 4 :path \"/tmp/photo.png\")").unwrap();
            assert_eq!(cmd.name, "file-add");
            assert_eq!(request_arg(&cmd).as_deref(), Some("4"));
            assert_eq!(string_arg(&cmd, "path").as_deref(), Some("/tmp/photo.png"));

            let outcome = AttachmentOutcome {
                kind: "image",
                path: PathBuf::from("/tmp/photo.png"),
                bytes: 128,
                mime: Some("image/png".to_string()),
                message: "attached image: /tmp/photo.png (128 B)".to_string(),
            };
            assert!(outcome.fields().contains(" :kind image"));
            assert!(outcome.fields().contains(" :mime \"image/png\""));

            let items = vec![AttachmentItem {
                index: 0,
                kind: "context-file",
                path: PathBuf::from("/tmp/notes.txt"),
                bytes: 12,
                mime: None,
            }];
            assert_eq!(
                attachment_items_to_sexp(&items),
                "((:index 0 :kind context-file :path \"/tmp/notes.txt\" :bytes 12))",
            );
        }

        #[test]
        fn session_render_lines_include_message_source_metadata() {
            let mut session = Session::new("openai", "gpt", 1000);
            session.add_message(MessageRole::User, "hello");
            session.add_message(MessageRole::Assistant, "world");
            let cli = Cli::default();
            let cfg = Config::default();
            let context = crate::context::load(true);
            let lines = render_session_lines(&session, &cli, &cfg, &context, 100);
            let encoded = lines_to_sexp(&lines);

            assert!(encoded.contains(":message-index 0 :role user"));
            assert!(encoded.contains(":message-index 1 :role assistant"));
        }

        #[test]
        fn session_render_lines_include_persisted_tool_events() {
            let mut session = Session::new("openai", "gpt", 1000);
            session.add_tool_call("bash", &serde_json::json!({ "command": "echo hi" }));
            session.add_tool_result("bash", "hi\n");
            session.add_subagent_tool_call("task", &serde_json::json!({ "prompts": ["find x"] }));
            let cli = Cli::default();
            let cfg = Config::default();
            let context = crate::context::load(true);
            let encoded = lines_to_sexp(&render_session_lines(&session, &cli, &cfg, &context, 100));

            assert!(encoded.contains(":role tool-call"));
            assert!(encoded.contains("◈"));
            assert!(encoded.contains(":role tool-result"));
            assert!(encoded.contains("hi"));
            assert!(encoded.contains(":role subagent-tool-call"));
            assert!(encoded.contains("⌥"));
        }

        #[test]
        fn tool_output_artifact_content_matches_session_transcript() {
            let mut session = Session::new("openai", "gpt", 1000);

            let content = tool_result_artifact_content(&mut session, "bash", "hi\n");

            assert_eq!(content, "bash:\nhi\n");
            assert_eq!(session.messages[0].content.as_str(), content);
        }

        #[test]
        fn with_source_lines_adds_message_source_metadata() {
            let encoded = lines_to_sexp(&with_source_lines(
                render_user_lines("hello"),
                3,
                MessageRole::User,
            ));

            assert!(encoded.contains(":message-index 3 :role user"));
        }

        #[test]
        fn agent_ended_error_is_only_for_current_running_turn() {
            let mutable = MutableState {
                seq: 0,
                cols: DEFAULT_COLS,
                line_count: 0,
                running: true,
                reasoning_enabled: true,
                reasoning_effort: None,
                abort_handle: None,
                turn: 2,
                #[cfg(feature = "loop")]
                loop_state: None,
                next_artifact_id: 1,
                next_permission_id: 1,
                pending_permissions: HashMap::new(),
                last_event_at: None,
            };

            assert!(!should_report_agent_ended(&mutable, 1));
            assert!(should_report_agent_ended(&mutable, 2));

            let stopped = MutableState {
                running: false,
                ..mutable
            };
            assert!(!should_report_agent_ended(&stopped, 2));
        }

        #[test]
        fn provider_and_model_commands_read_values() {
            let provider =
                parse_command("(provider :request 5 :provider \"openai-codex\")").unwrap();
            assert_eq!(provider.name, "provider");
            assert_eq!(request_arg(&provider).as_deref(), Some("5"));
            assert_eq!(
                string_arg(&provider, "provider").as_deref(),
                Some("openai-codex")
            );

            let model = parse_command("(model :request 6 :model \"gpt-5.5\")").unwrap();
            assert_eq!(model.name, "model");
            assert_eq!(request_arg(&model).as_deref(), Some("6"));
            assert_eq!(string_arg(&model, "model").as_deref(), Some("gpt-5.5"));

            let thinking = parse_command("(thinking :request 7 :level off)").unwrap();
            assert_eq!(thinking.name, "thinking");
            assert_eq!(request_arg(&thinking).as_deref(), Some("7"));
            assert_eq!(atom_arg(&thinking, "level").as_deref(), Some("off"));
        }

        #[cfg(feature = "loop")]
        #[test]
        fn loop_start_command_reads_settings_and_serializes_status() {
            let cmd = parse_command(
                "(loop-start :request 8 :prompt \"fix bugs\" :max 3 :plan \"PLAN.md\" :run \"cargo test\")",
            )
		.unwrap();
            assert_eq!(cmd.name, "loop-start");
            assert_eq!(request_arg(&cmd).as_deref(), Some("8"));
            assert_eq!(string_arg(&cmd, "prompt").as_deref(), Some("fix bugs"));
            assert_eq!(u64_arg(&cmd, "max"), Some(3));
            assert_eq!(string_arg(&cmd, "plan").as_deref(), Some("PLAN.md"));
            assert_eq!(string_arg(&cmd, "run").as_deref(), Some("cargo test"));

            let mut loop_state = crate::extras::r#loop::LoopState::new(
                "fix bugs".to_string(),
                PathBuf::from("PLAN.md"),
                Some(3),
                Some("cargo test".to_string()),
            );
            loop_state.iteration = 2;
            let fields = loop_fields(&loop_state);
            assert!(fields.contains(" :active t"));
            assert!(fields.contains(" :iteration 2"));
            assert!(fields.contains(" :label \"LOOP 2/3\""));
            assert!(fields.contains(" :max 3"));
            assert!(fields.contains(" :plan \"PLAN.md\""));
            assert!(fields.contains(" :prompt \"fix bugs\""));
        }

        #[test]
        fn rendered_lines_are_plists() {
            let lines = vec![WireLine::new("< hi", "zs-normal")];
            assert_eq!(lines_to_sexp(&lines), "((:text \"< hi\" :face zs-normal))");
        }

        #[test]
        fn assistant_markdown_includes_line_local_spans() {
            let lines = render_assistant_lines(
                "# Head\n\n**bold** *italic* `code` [link](https://example.invalid)\n\n> quote\n\n- [x] task\n\n```rust\nfn main() {}\n```",
                100,
                false,
            );
            let rendered = lines_to_sexp(&lines);
            assert!(rendered.contains(":spans"));
            assert!(rendered.contains(":face zs-heading"));
            assert!(rendered.contains(":face zs-bold"));
            assert!(rendered.contains(":face zs-italic"));
            assert!(rendered.contains(":face zs-code"));
            assert!(rendered.contains(":face zs-link"));
            assert!(rendered.contains(":face zs-quote"));
            assert!(rendered.contains(":face zs-list-marker"));
            assert!(rendered.contains(":face zs-code-block"));
        }

        #[test]
        fn assistant_markdown_tables_are_boxed_and_aligned() {
            let lines = render_assistant_lines(
                "| Left | Center | Right |\n| :--- | :----: | ----: |\n| a | bb | 3 |\n| longer | c | 400 |",
                100,
                false,
            );
            let rendered = lines_to_sexp(&lines);
            assert!(rendered.contains(":face zs-table-border"));
            assert!(rendered.contains(":face zs-table"));
            assert!(rendered.contains("┌"));
            assert!(rendered.contains("├"));
            assert!(rendered.contains("└"));
            assert!(rendered.contains("│ a      │   bb   │     3 │"));
            assert!(rendered.contains("│ longer │   c    │   400 │"));
        }

        #[test]
        fn rendered_lines_can_link_ephemeral_artifacts() {
            let artifact = ArtifactInfo {
                kind: "tool-output",
                path: PathBuf::from("/tmp/zs/artifacts/turn-1/0001-bash.txt"),
                mime: "text/plain; charset=utf-8",
                bytes: 12,
                preview: "hello world".to_string(),
            };
            let lines = vec![WireLine::with_artifact(
                "  output: bash (12 B)",
                "zs-link",
                artifact,
            )];
            assert_eq!(
                lines_to_sexp(&lines),
                "((:text \"  output: bash (12 B)\" :face zs-link :artifact (:kind tool-output :path \"/tmp/zs/artifacts/turn-1/0001-bash.txt\" :mime \"text/plain; charset=utf-8\" :bytes 12 :preview \"hello world\" :ephemeral t :expires process-exit)))"
            );
        }

        #[test]
        fn tool_calls_start_a_new_reasoning_render_segment() {
            let mut reasoning_start_line = Some(4);
            reset_reasoning_render_segment(&mut reasoning_start_line);
            assert_eq!(reasoning_start_line, None);
        }

        #[test]
        fn latex_spans_detect_inline_and_display_math() {
            let lines = vec![
                WireLine::new("< Inline $x^2$ here", "zs-normal"),
                WireLine::new("$$", "zs-normal"),
                WireLine::new("y = mx + b", "zs-normal"),
                WireLine::new("$$", "zs-normal"),
            ];

            let spans = find_latex_spans(&lines);
            assert_eq!(spans.len(), 2);
            assert_eq!(spans[0].source, "x^2");
            assert!(!spans[0].display);
            assert_eq!(spans[1].source, "y = mx + b");
            assert!(spans[1].display);
            assert_eq!(spans[1].line_start, 1);
            assert_eq!(spans[1].line_end, 3);
        }

        #[test]
        fn rendered_lines_include_latex_metadata() {
            let artifact = ArtifactInfo {
                kind: "latex-source",
                path: PathBuf::from("/tmp/zs/artifacts/turn-2/latex-0001.tex"),
                mime: "text/x-tex; charset=utf-8",
                bytes: 120,
                preview: "\\documentclass{article}".to_string(),
            };
            let svg_artifact = ArtifactInfo {
                kind: "latex-svg",
                path: PathBuf::from("/tmp/zs/artifacts/turn-2/latex-0001.svg"),
                mime: "image/svg+xml",
                bytes: 240,
                preview: "<svg".to_string(),
            };
            let latex = LatexInfo {
                id: "turn-2-latex-1".to_string(),
                source: "x^2".to_string(),
                display: false,
                artifact,
                svg_artifact: Some(svg_artifact),
                line_start: 42,
                col_start: 9,
                line_end: 42,
                col_end: 14,
            };
            let mut line = WireLine::new("< Inline $x^2$", "zs-normal");
            line.push_latex(latex.clone());

            let rendered = lines_to_sexp(&[line]);
            assert!(rendered.contains(":latex ((:id \"turn-2-latex-1\""));
            assert!(rendered.contains(":display nil"));
            assert!(rendered.contains(":source \"x^2\""));
            assert!(rendered.contains(":line-start 42 :col-start 9 :line-end 42 :col-end 14"));
            let latex_sexp = latex_items_to_sexp(&[latex]);
            assert!(latex_sexp.contains(":artifact (:kind latex-source"));
            assert!(latex_sexp.contains(":svg-artifact (:kind latex-svg"));
            assert!(latex_sexp.contains(":mime \"image/svg+xml\""));
        }

        #[test]
        fn latex_document_wraps_for_auctex_preview() {
            let inline = latex_document("x^2", false);
            assert!(
                inline.contains("\\usepackage[active,tightpage,displaymath,textmath]{preview}")
            );
            assert!(inline.contains("\\(x^2\\)"));

            let display = latex_document("y = mx + b", true);
            assert!(display.contains("\\[\ny = mx + b\n\\]"));
        }

        #[test]
        fn artifact_helpers_keep_preview_and_filename_small() {
            assert_eq!(preview_text("hello\n\tworld"), "hello world");
            assert_eq!(safe_filename("Bash Tool/Output!"), "bash-tool-output");
            assert_eq!(format_bytes(1536), "1.5 KB");
        }

        #[test]
        fn stale_session_dirs_are_removed_when_listing() {
            let root =
                std::env::temp_dir().join(format!("zs_emacs_test_{}_stale", std::process::id()));
            let dir = root.join("session-id");
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("meta.json"), "{}").unwrap();

            let sessions = list_sessions_in(&root, true).unwrap();
            assert!(sessions.is_empty());
            assert!(!dir.exists());

            let _ = std::fs::remove_dir_all(&root);
        }

        #[tokio::test]
        async fn abort_allows_provider_to_receive_next_prompt() {
            let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
            let (server, registration, listener) = test_server(prompts.clone());
            drop(listener);
            let (out_tx, mut out_rx) = mpsc::channel(16);
            let sleep_cmd = parse_command("(prompt :request 1 :text \"sleep\")").unwrap();
            handle_prompt(server.clone(), &sleep_cmd, &out_tx)
                .await
                .unwrap();
            wait_for_prompt(&prompts, "sleep").await;
            wait_for_active_command(&server).await;
            let abort_cmd = parse_command("(abort :request 2)").unwrap();
            handle_abort(&server, &abort_cmd, &out_tx).await.unwrap();
            wait_for_not_running(&server).await;
            let after_cmd = parse_command("(prompt :request 3 :text \"after\")").unwrap();
            handle_prompt(server.clone(), &after_cmd, &out_tx)
                .await
                .unwrap();
            wait_for_prompt(&prompts, "after").await;
            let seen = prompts.lock().unwrap_or_else(|e| e.into_inner()).clone();
            assert_eq!(seen, vec!["sleep".to_string(), "after".to_string()]);
            while out_rx.try_recv().is_ok() {}
            let _ = std::fs::remove_dir_all(&registration.dir);
        }

        #[tokio::test]
        async fn abort_over_socket_allows_provider_to_receive_next_prompt() {
            let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
            let (server, registration, listener) = test_server(prompts.clone());
            let socket_path = registration.socket_path.clone();
            let accept_task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                handle_client(server, stream).await;
            });

            let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader).lines();
            let _ = read_until(&mut reader, "ready", Duration::from_secs(1)).await;

            writer
                .write_all(b"(prompt :request 1 :text \"sleep\")\n")
                .await
                .unwrap();
            wait_for_prompt(&prompts, "sleep").await;
            let _ = read_until(&mut reader, "tool-call", Duration::from_secs(2)).await;

            writer.write_all(b"(abort :request 2)\n").await.unwrap();
            let _ = read_until(&mut reader, "aborted", Duration::from_secs(2)).await;

            writer
                .write_all(b"(prompt :request 3 :text \"after\")\n")
                .await
                .unwrap();
            wait_for_prompt(&prompts, "after").await;
            let seen = prompts.lock().unwrap_or_else(|e| e.into_inner()).clone();
            assert_eq!(seen, vec!["sleep".to_string(), "after".to_string()]);
            drop(writer);
            let _ = timeout(Duration::from_secs(1), accept_task).await;
            let _ = std::fs::remove_dir_all(&registration.dir);
        }

        fn test_server(
            prompts: Arc<std::sync::Mutex<Vec<String>>>,
        ) -> (Arc<Server>, Registration, UnixListener) {
            let client = AnyClient::Test(crate::provider::TestClient { prompts });
            let session = Session::new("test", "test", 0);
            let (registration, listener) = Registration::create(&session).unwrap();
            let socket_path = registration.socket_path.clone();
            let server = Arc::new(Server {
                client: Mutex::new(client),
                cli: Cli {
                    no_session: true,
                    ..Cli::default()
                },
                cfg: Config::default(),
                context: Mutex::new(crate::context::load(true)),
                session: Mutex::new(session),
                permission: None,
                ask_tx: None,
                sandbox: Sandbox::new(false, "bwrap"),
                status_signals: None,
                #[cfg(feature = "mcp")]
                mcp_manager: Mutex::new(None),
                events: broadcast::channel(EVENT_BUFFER).0,
                mutable: Mutex::new(MutableState {
                    seq: 0,
                    cols: DEFAULT_COLS,
                    line_count: 0,
                    running: false,
                    reasoning_enabled: true,
                    reasoning_effort: None,
                    abort_handle: None,
                    turn: 0,
                    #[cfg(feature = "loop")]
                    loop_state: None,
                    next_artifact_id: 1,
                    next_permission_id: 1,
                    pending_permissions: HashMap::new(),
                    last_event_at: None,
                }),
                registry_dir: registration.dir.clone(),
                socket_path,
            });
            (server, registration, listener)
        }

        async fn read_until(
            reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
            needle: &str,
            duration: Duration,
        ) -> String {
            timeout(duration, async {
                loop {
                    let line = reader.next_line().await.unwrap().unwrap();
                    if line.contains(needle) {
                        return line;
                    }
                }
            })
            .await
            .unwrap()
        }

        async fn wait_for_prompt(prompts: &Arc<std::sync::Mutex<Vec<String>>>, prompt: &str) {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if prompts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .any(|p| p == prompt)
                {
                    return;
                }
                assert!(std::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        async fn wait_for_not_running(server: &Arc<Server>) {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if !server.mutable.lock().await.running {
                    return;
                }
                assert!(std::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        async fn wait_for_active_command(server: &Arc<Server>) {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if server.sandbox.active_group_count() == 1 {
                    return;
                }
                assert!(std::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

#[cfg(unix)]
pub use imp::{print_sessions, serve};

#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    _client: crate::provider::AnyClient,
    _cli: crate::cli::Cli,
    _cfg: crate::config::Config,
    _context: crate::context::ContextFiles,
    _session: crate::session::Session,
    _permission: Option<crate::permission::checker::PermCheck>,
    _ask_tx: Option<crate::permission::ask::AskSender>,
    _ask_rx: Option<crate::permission::ask::AskReceiver>,
    _sandbox: crate::sandbox::Sandbox,
    _status_signals: Option<crate::extras::status_signals::StatusSignals>,
) -> anyhow::Result<()> {
    anyhow::bail!("native Emacs protocol requires Unix sockets")
}

#[cfg(not(unix))]
pub fn print_sessions() -> anyhow::Result<()> {
    anyhow::bail!("native Emacs protocol requires Unix sockets")
}
