use crate::permission::checker::{CheckResult, PermissionChecker};
use crate::permission::{Action, PermissionConfig, PermissionConfigs, SecurityMode, ToolPerm};

fn default_modes() -> Option<Vec<String>> {
    Some(vec![
        "guarded".to_string(),
        "standard".to_string(),
        "yolo".to_string(),
    ])
}

fn make_checker(mode: SecurityMode) -> PermissionChecker {
    PermissionChecker::new(
        &PermissionConfigs::default(),
        mode,
        Some(std::path::PathBuf::from("/home/user/project")),
        default_modes(),
    )
}

fn make_checker_with_modes(mode: SecurityMode, modes: Option<Vec<String>>) -> PermissionChecker {
    PermissionChecker::new(
        &PermissionConfigs::default(),
        mode,
        Some(std::path::PathBuf::from("/home/user/project")),
        modes,
    )
}

fn configs_from(config: PermissionConfig) -> PermissionConfigs {
    PermissionConfigs::from(config)
}

// --- SecurityMode behavior ---

#[test]
fn yolo_allows_non_destructive_and_write_edit() {
    let mut checker = make_checker(SecurityMode::Yolo);
    assert_eq!(checker.check("write", "/etc/passwd"), CheckResult::Allowed);
    assert_eq!(checker.check("edit", "src/main.rs"), CheckResult::Allowed);
    assert_eq!(checker.check("read", "/etc/config"), CheckResult::Allowed);
    assert_eq!(checker.check("bash", "ls"), CheckResult::Allowed);
}

#[test]
fn yolo_asks_for_destructive_bash() {
    let mut checker = make_checker(SecurityMode::Yolo);
    // Destructive commands like rm are Deny in default rules → converted to Ask
    let result = checker.check("bash", "rm -rf /");
    assert!(
        matches!(result, CheckResult::Ask),
        "expected Ask for rm -rf / in YOLO, got {:?}",
        result,
    );
}

#[test]
fn yolo_asks_for_destructive_bash_with_pattern() {
    let mut checker = make_checker(SecurityMode::Yolo);
    let result = checker.check("bash", "dd if=/dev/zero of=/dev/sda");
    assert!(
        matches!(result, CheckResult::Ask),
        "expected Ask for dd in YOLO, got {:?}",
        result,
    );
}

#[test]
fn restrictive_makes_unconfigured_tool_ask() {
    let mut checker = make_checker(SecurityMode::Restrictive);
    let result = checker.check("some_tool", "any input");
    assert!(matches!(result, CheckResult::Ask));
}

#[test]
fn standard_allows_unknown_tool_with_default() {
    let mut checker = make_checker(SecurityMode::Standard);
    let result = checker.check("some_tool", "any input");
    assert!(matches!(result, CheckResult::Allowed));
}

// --- ReadOnly mode ---

#[test]
fn readonly_allows_read_tools() {
    let mut checker = make_checker(SecurityMode::ReadOnly);
    assert!(matches!(
        checker.check("read", "/etc/passwd"),
        CheckResult::Allowed
    ));
    assert!(matches!(
        checker.check("grep", "pattern"),
        CheckResult::Allowed
    ));
    assert!(matches!(
        checker.check("find_files", "*.rs"),
        CheckResult::Allowed
    ));
    assert!(matches!(
        checker.check("list_dir", "/home/user"),
        CheckResult::Allowed
    ));
}

#[test]
fn readonly_denies_write_bash_and_edit() {
    let mut checker = make_checker(SecurityMode::ReadOnly);
    assert!(matches!(
        checker.check("write", "/etc/passwd"),
        CheckResult::Denied(_)
    ));
    assert!(matches!(
        checker.check("edit", "src/main.rs"),
        CheckResult::Denied(_)
    ));
    assert!(matches!(
        checker.check("bash", "ls"),
        CheckResult::Denied(_)
    ));
    assert!(matches!(
        checker.check("bash", "rm -rf /"),
        CheckResult::Denied(_)
    ));
    assert!(matches!(
        checker.check("write_todo_list", ""),
        CheckResult::Denied(_)
    ));
}

#[test]
fn readonly_denies_path_tools_outside_read() {
    let mut checker = make_checker(SecurityMode::ReadOnly);
    assert!(matches!(
        checker.check_path("write", "/home/user/project/new.rs"),
        CheckResult::Denied(_),
    ));
    assert!(matches!(
        checker.check_path("edit", "/home/user/project/src/main.rs"),
        CheckResult::Denied(_),
    ));
}

// --- Guarded mode ---

#[test]
fn guarded_allows_read_tools() {
    let mut checker = make_checker(SecurityMode::Guarded);
    assert!(matches!(
        checker.check("read", "/etc/passwd"),
        CheckResult::Allowed
    ));
    assert!(matches!(
        checker.check("grep", "pattern"),
        CheckResult::Allowed
    ));
    assert!(matches!(
        checker.check("list_dir", "/home/user"),
        CheckResult::Allowed
    ));
}

#[test]
fn guarded_asks_for_write_and_bash() {
    let mut checker = make_checker(SecurityMode::Guarded);
    assert!(matches!(
        checker.check("write", "/etc/passwd"),
        CheckResult::Ask
    ));
    assert!(matches!(
        checker.check("edit", "src/main.rs"),
        CheckResult::Ask
    ));
    // Bash: no default rule matches (it's a different pattern)
    assert!(matches!(checker.check("bash", "wget"), CheckResult::Ask));
    // But configured defaults like ls still apply
    assert!(matches!(
        checker.check("bash", "ls -la"),
        CheckResult::Allowed
    ));
}

// --- Deny rules ---

#[test]
fn deny_rule_blocks_regardless_of_mode() {
    let mut checker = make_checker(SecurityMode::Standard);
    let result = checker.check("bash", "rm -rf /home/user/project");
    assert!(matches!(result, CheckResult::Denied(_)));
}

#[test]
fn deny_rule_is_asked_in_yolo() {
    let mut checker = make_checker(SecurityMode::Yolo);
    let result = checker.check("bash", "rm -rf /home/user/project");
    assert!(
        matches!(result, CheckResult::Ask),
        "expected Ask for destructive bash in YOLO, got {:?}",
        result,
    );
}

// --- Doom loop detection ---

#[test]
fn doom_loop_triggers_after_three_repeated_calls() {
    let mut checker = make_checker(SecurityMode::Standard);
    checker.check("bash", "ls");
    checker.check("bash", "ls");
    let result = checker.check("bash", "ls");
    assert!(matches!(result, CheckResult::Ask));
}

#[test]
fn doom_loop_does_not_trigger_before_three() {
    let mut checker = make_checker(SecurityMode::Standard);
    checker.check("bash", "ls");
    let result = checker.check("bash", "ls");
    assert!(matches!(result, CheckResult::Allowed));
}

#[test]
fn doom_loop_resets_for_different_inputs() {
    let mut checker = make_checker(SecurityMode::Standard);
    checker.check("bash", "ls");
    checker.check("bash", "ls");
    checker.check("bash", "pwd");
    let result = checker.check("bash", "pwd");
    assert!(matches!(result, CheckResult::Allowed));
}

// --- Session allowlist ---

#[test]
fn session_allowlist_bypasses_rules() {
    let mut checker = make_checker(SecurityMode::Restrictive);
    checker.add_session_allowlist("bash".into(), "cargo test **");
    let result = checker.check("bash", "cargo test --all");
    assert!(matches!(result, CheckResult::Allowed));
}

#[test]
fn session_allowlist_is_tool_specific() {
    let mut checker = make_checker(SecurityMode::Restrictive);
    checker.add_session_allowlist("read".into(), "**");
    assert!(matches!(
        checker.check("read", "/etc/passwd"),
        CheckResult::Allowed
    ));
    assert!(matches!(
        checker.check("write", "some/file.txt"),
        CheckResult::Ask
    ));
}

// --- External path detection ---

#[test]
fn external_absolute_path_outside_cwd_is_detected() {
    let mut checker = make_checker(SecurityMode::Standard);
    let external_path = if cfg!(windows) {
        "D:\\outside\\secret.txt"
    } else {
        "/etc/shadow"
    };
    let result = checker.check_path("write", external_path);
    assert!(
        matches!(result, CheckResult::Ask),
        "expected Ask, got {:?}",
        result,
    );
}

#[test]
fn relative_path_is_not_external() {
    let mut checker = make_checker(SecurityMode::Standard);
    let result = checker.check_path("read", "src/lib.rs");
    assert!(matches!(result, CheckResult::Allowed));
}

// --- Config-driven rules ---

#[test]
fn explicit_granular_rules_take_effect() {
    let config = PermissionConfig {
        read: Some(ToolPerm::Granular(
            [
                ("*.md".to_string(), Action::Allow),
                ("*.rs".to_string(), Action::Ask),
            ]
            .into(),
        )),
        ..PermissionConfig::default()
    };
    let mut checker = PermissionChecker::new(
        &configs_from(config),
        SecurityMode::Standard,
        None,
        default_modes(),
    );
    assert_eq!(checker.check("read", "README.md"), CheckResult::Allowed);
    assert_eq!(checker.check("read", "main.rs"), CheckResult::Ask);
}

// --- Standard mode: allow path tools in CWD only when no rule matches ---

#[test]
fn standard_path_tools_in_cwd_without_rules_are_allowed() {
    let mut checker = make_checker(SecurityMode::Standard);
    assert!(matches!(
        checker.check_path("read", "/home/user/project/src/main.rs"),
        CheckResult::Allowed,
    ));
    assert!(matches!(
        checker.check_path("write", "/home/user/project/new_file.rs"),
        CheckResult::Allowed,
    ));
    assert!(matches!(
        checker.check_path("list_dir", "/home/user/project/src"),
        CheckResult::Allowed,
    ));
}

#[test]
fn standard_respects_deny_rules_for_path_tools_in_cwd() {
    // Config rules are more dominant than mode defaults, so explicit Deny rules win.
    // Use ** pattern to match paths with slashes.
    let config = PermissionConfig {
        read: Some(ToolPerm::Granular(
            [("**".to_string(), Action::Deny)].into(),
        )),
        ..PermissionConfig::default()
    };
    let mut checker = PermissionChecker::new(
        &configs_from(config),
        SecurityMode::Standard,
        Some(std::path::PathBuf::from("/home/user/project")),
        default_modes(),
    );
    let result = checker.check_path("read", "/home/user/project/src/main.rs");
    assert!(
        matches!(result, CheckResult::Denied(_)),
        "expected Denied for CWD path with explicit deny rule, got {:?}",
        result,
    );
}

#[test]
fn standard_respects_deny_rules_for_write_in_cwd() {
    let config = PermissionConfig {
        write: Some(ToolPerm::Granular(
            [("**".to_string(), Action::Deny)].into(),
        )),
        ..PermissionConfig::default()
    };
    let mut checker = PermissionChecker::new(
        &configs_from(config),
        SecurityMode::Standard,
        Some(std::path::PathBuf::from("/home/user/project")),
        default_modes(),
    );
    let result = checker.check_path("write", "/home/user/project/new_file.rs");
    assert!(
        matches!(result, CheckResult::Denied(_)),
        "expected Denied for CWD write with explicit deny rule, got {:?}",
        result,
    );
}

#[test]
fn standard_asks_external_path_even_for_path_tools() {
    // External paths should still trigger Ask in Standard mode
    let mut checker = make_checker(SecurityMode::Standard);
    let external = if cfg!(windows) {
        "D:\\outside\\file.txt"
    } else {
        "/etc/config.conf"
    };
    let result = checker.check_path("read", external);
    assert!(
        matches!(result, CheckResult::Ask),
        "expected Ask for external path, got {:?}",
        result,
    );
}

#[test]
fn standard_deny_still_works_for_non_path_tools() {
    // Non-path tools (bash, grep, etc.) should still respect deny rules
    let mut checker = make_checker(SecurityMode::Standard);
    let result = checker.check("bash", "rm -rf /home/user/project");
    assert!(
        matches!(result, CheckResult::Denied(_)),
        "expected Denied for bash deny rule, got {:?}",
        result,
    );
}

#[test]
fn standard_allows_configured_bash_commands() {
    let mut checker = make_checker(SecurityMode::Standard);
    assert!(matches!(
        checker.check("bash", "ls -la"),
        CheckResult::Allowed
    ));
    assert!(matches!(
        checker.check("bash", "git status"),
        CheckResult::Allowed
    ));
    assert!(matches!(
        checker.check("bash", "cargo build"),
        CheckResult::Allowed
    ));
}

// --- Regex permission rules ---

#[test]
fn regex_granular_rules_take_effect() {
    let config = PermissionConfig {
        read: Some(ToolPerm::Granular(
            [
                (r"\.md$".to_string(), Action::Allow),
                (r"\.rs$".to_string(), Action::Ask),
            ]
            .into(),
        )),
        ..PermissionConfig::default()
    };
    let configs = PermissionConfigs {
        regex: config,
        ..PermissionConfigs::default()
    };
    let mut checker =
        PermissionChecker::new(&configs, SecurityMode::Standard, None, default_modes());
    assert_eq!(checker.check("read", "README.md"), CheckResult::Allowed);
    assert_eq!(checker.check("read", "main.rs"), CheckResult::Ask);
    assert_eq!(checker.check("read", "main.py"), CheckResult::Allowed);
}

#[test]
fn regex_simple_action() {
    let config = PermissionConfig {
        bash: Some(ToolPerm::Simple(Action::Ask)),
        ..PermissionConfig::default()
    };
    let configs = PermissionConfigs {
        regex: config,
        ..PermissionConfigs::default()
    };
    let mut checker =
        PermissionChecker::new(&configs, SecurityMode::Standard, None, default_modes());
    let result = checker.check("bash", "anything");
    assert!(matches!(result, CheckResult::Ask));
}

#[test]
fn regex_and_glob_rules_merge() {
    let glob = PermissionConfig {
        read: Some(ToolPerm::Granular(
            [("*.md".to_string(), Action::Allow)].into(),
        )),
        ..PermissionConfig::default()
    };
    let regex = PermissionConfig {
        read: Some(ToolPerm::Granular(
            [(r"\.rs$".to_string(), Action::Ask)].into(),
        )),
        ..PermissionConfig::default()
    };
    let configs = PermissionConfigs { glob, regex };
    let mut checker =
        PermissionChecker::new(&configs, SecurityMode::Standard, None, default_modes());
    assert_eq!(checker.check("read", "README.md"), CheckResult::Allowed);
    assert_eq!(checker.check("read", "main.rs"), CheckResult::Ask);
}

#[test]
fn regex_default_action_used_when_no_glob_default() {
    let glob = PermissionConfig::default();
    let regex = PermissionConfig {
        default: Some(Action::Ask),
        ..PermissionConfig::default()
    };
    let configs = PermissionConfigs { glob, regex };
    let mut checker =
        PermissionChecker::new(&configs, SecurityMode::Standard, None, default_modes());
    // Default from regex config should be used when glob has no default
    let result = checker.check("unknown_tool", "anything");
    assert!(matches!(result, CheckResult::Ask));
}

#[test]
fn regex_glob_default_precedence() {
    let glob = PermissionConfig {
        default: Some(Action::Allow),
        ..PermissionConfig::default()
    };
    let regex = PermissionConfig {
        default: Some(Action::Ask),
        ..PermissionConfig::default()
    };
    let configs = PermissionConfigs { glob, regex };
    let mut checker =
        PermissionChecker::new(&configs, SecurityMode::Standard, None, default_modes());
    // Glob default should take precedence over regex default
    let result = checker.check("unknown_tool", "anything");
    assert!(matches!(result, CheckResult::Allowed));
}

// --- Path traversal detection (normalize_path) ---

#[test]
fn path_traversal_with_dotdot_is_detected_as_external() {
    let mut checker = make_checker(SecurityMode::Standard);
    let traversal = if cfg!(windows) {
        "C:\\home\\user\\project\\..\\etc\\shadow"
    } else {
        "/home/user/project/../etc/shadow"
    };
    let result = checker.check_path("read", traversal);
    assert!(
        matches!(result, CheckResult::Ask),
        "expected Ask for traversal path, got {:?}",
        result,
    );
}

#[test]
fn dot_components_are_normalized_away() {
    let mut checker = make_checker(SecurityMode::Standard);
    let path = if cfg!(windows) {
        "C:\\home\\user\\project\\.\\src\\main.rs"
    } else {
        "/home/user/project/./src/main.rs"
    };
    let result = checker.check_path("read", path);
    assert!(
        matches!(result, CheckResult::Allowed),
        "expected Allowed for dot-normalized CWD path, got {:?}",
        result,
    );
}

#[test]
fn nested_dotdot_traverses_to_root() {
    let mut checker = make_checker(SecurityMode::Standard);
    let traversal = if cfg!(windows) {
        "C:\\home\\user\\project\\..\\..\\..\\etc\\passwd"
    } else {
        "/home/user/project/../../../etc/passwd"
    };
    let result = checker.check_path("read", traversal);
    assert!(
        matches!(result, CheckResult::Ask),
        "expected Ask for deep traversal path, got {:?}",
        result,
    );
}

// --- Session allowlist with absolute paths on check_path ---

#[test]
fn session_allowlist_matches_absolute_path_when_stored_as_relative() {
    let mut checker = make_checker(SecurityMode::Restrictive);
    checker.add_session_allowlist("read".into(), "src/*");
    let result = checker.check_path("read", "/home/user/project/src/main.rs");
    assert!(
        matches!(result, CheckResult::Allowed),
        "expected Allowed for absolute path matching relative allowlist, got {:?}",
        result,
    );
}

#[test]
fn session_allowlist_matches_relative_path_when_stored_as_absolute() {
    let mut checker = make_checker(SecurityMode::Restrictive);
    checker.add_session_allowlist("read".into(), "/home/user/project/src/*");
    let result = checker.check_path("read", "src/main.rs");
    assert!(
        matches!(result, CheckResult::Allowed),
        "expected Allowed for relative path matching absolute allowlist, got {:?}",
        result,
    );
}

// --- MCP tool config ---

#[test]
fn mcp_tool_simple_rule_is_respected() {
    let config = PermissionConfig {
        mcp_tool: Some(ToolPerm::Simple(Action::Deny)),
        ..PermissionConfig::default()
    };
    let mut checker = PermissionChecker::new(
        &configs_from(config),
        SecurityMode::Standard,
        None,
        default_modes(),
    );
    let result = checker.check("mcp_tool", "mcp_tool:filesystem:read_file");
    assert!(
        matches!(result, CheckResult::Denied(_)),
        "expected Denied for MCP tool, got {:?}",
        result,
    );
}

#[test]
fn mcp_tool_granular_rules_respected() {
    let config = PermissionConfig {
        mcp_tool: Some(ToolPerm::Granular(
            [
                ("mcp_tool:fs:allow_*".to_string(), Action::Allow),
                ("mcp_tool:fs:deny_*".to_string(), Action::Deny),
            ]
            .into(),
        )),
        ..PermissionConfig::default()
    };
    let mut checker = PermissionChecker::new(
        &configs_from(config),
        SecurityMode::Standard,
        None,
        default_modes(),
    );
    assert_eq!(
        checker.check("mcp_tool", "mcp_tool:fs:allow_read"),
        CheckResult::Allowed
    );
    assert!(matches!(
        checker.check("mcp_tool", "mcp_tool:fs:deny_write"),
        CheckResult::Denied(_)
    ));
}

#[test]
fn mcp_tool_default_action_when_no_rules() {
    let mut checker = make_checker(SecurityMode::Standard);
    let result = checker.check("mcp_tool", "mcp_tool:some_server:some_tool");
    assert!(
        matches!(result, CheckResult::Allowed),
        "expected Allowed for MCP tool with no rules (default), got {:?}",
        result,
    );
}

// --- Restricted mode: ask for everything ---

#[test]
fn restrictive_asks_for_everything() {
    // With default modes (Restrictive not in list), no rules apply -> always Ask
    let mut checker = make_checker(SecurityMode::Restrictive);
    assert!(matches!(
        checker.check("read", "anything"),
        CheckResult::Ask
    ));
    assert!(matches!(
        checker.check("write", "anything"),
        CheckResult::Ask
    ));
    assert!(matches!(checker.check("bash", "ls"), CheckResult::Ask));
    assert!(matches!(
        checker.check("bash", "rm -rf /"),
        CheckResult::Ask
    ));
}

#[test]
fn restrictive_with_rules_in_permission_modes_respects_matched() {
    // When Restrictive is explicitly added to permission_modes, matched rules are respected.
    // Use ** pattern to match inputs with slashes.
    let config = PermissionConfig {
        read: Some(ToolPerm::Granular(
            [("**".to_string(), Action::Allow)].into(),
        )),
        ..PermissionConfig::default()
    };
    let mut checker = PermissionChecker::new(
        &configs_from(config),
        SecurityMode::Restrictive,
        Some(std::path::PathBuf::from("/home/user/project")),
        Some(vec!["restrictive".to_string(), "standard".to_string()]),
    );
    // read has an explicit Allow for ** -> Allowed
    assert!(matches!(
        checker.check("read", "/etc/passwd"),
        CheckResult::Allowed
    ));
    // write has no rule -> unmatched -> Ask
    assert!(matches!(
        checker.check("write", "anything"),
        CheckResult::Ask
    ));
}

// --- Permission modes filtering ---

#[test]
fn apply_rules_skipped_when_mode_not_in_permission_modes() {
    let config = PermissionConfig {
        bash: Some(ToolPerm::Granular(
            [("safe-*".to_string(), Action::Allow)].into(),
        )),
        ..PermissionConfig::default()
    };
    // Guarded is NOT in the modes list -> rules not applied
    let mut checker = PermissionChecker::new(
        &configs_from(config),
        SecurityMode::Guarded,
        Some(std::path::PathBuf::from("/home/user/project")),
        Some(vec!["standard".to_string()]),
    );
    // Without rules, Guarded asks for non-read tools
    let result = checker.check("bash", "safe-command");
    assert!(
        matches!(result, CheckResult::Ask),
        "expected Ask when rules are skipped by permission_modes, got {:?}",
        result,
    );
}

#[test]
fn apply_rules_applied_when_mode_in_permission_modes() {
    let config = PermissionConfig {
        bash: Some(ToolPerm::Granular(
            [("safe-*".to_string(), Action::Allow)].into(),
        )),
        ..PermissionConfig::default()
    };
    // Standard IS in the modes list -> rules apply
    let mut checker = PermissionChecker::new(
        &configs_from(config),
        SecurityMode::Standard,
        Some(std::path::PathBuf::from("/home/user/project")),
        Some(vec!["standard".to_string()]),
    );
    let result = checker.check("bash", "safe-command");
    assert!(
        matches!(result, CheckResult::Allowed),
        "expected Allowed when rules apply via permission_modes, got {:?}",
        result,
    );
}

// --- Guarded respects config rules ---

#[test]
fn guarded_respects_explicit_config_allow() {
    let config = PermissionConfig {
        bash: Some(ToolPerm::Granular(
            [("wget **".to_string(), Action::Allow)].into(),
        )),
        ..PermissionConfig::default()
    };
    let mut checker = PermissionChecker::new(
        &configs_from(config),
        SecurityMode::Guarded,
        Some(std::path::PathBuf::from("/home/user/project")),
        default_modes(),
    );
    // bash has an explicit Allow rule for wget -> Allowed
    assert!(matches!(
        checker.check("bash", "wget http://example.com"),
        CheckResult::Allowed
    ));
    // Other bash commands (no rule) -> Ask (mode default for non-read in Guarded)
    assert!(matches!(
        checker.check("bash", "unknown-cmd"),
        CheckResult::Ask
    ));
}

#[test]
fn guarded_respects_explicit_config_deny() {
    let config = PermissionConfig {
        read: Some(ToolPerm::Granular(
            [("*.secret".to_string(), Action::Deny)].into(),
        )),
        ..PermissionConfig::default()
    };
    let mut checker = PermissionChecker::new(
        &configs_from(config),
        SecurityMode::Guarded,
        Some(std::path::PathBuf::from("/home/user/project")),
        default_modes(),
    );
    // read has explicit Deny for .secret files -> Denied
    assert!(matches!(
        checker.check("read", "private.secret"),
        CheckResult::Denied(_)
    ));
    // Other reads (no rule) -> Allowed (read is a read tool)
    assert!(matches!(
        checker.check("read", "README.md"),
        CheckResult::Allowed
    ));
}

// --- Standard mode: external path handling with unmatched rules ---

#[test]
fn standard_external_path_with_default_allow_asks() {
    // Default allow (no config override) + external path = Ask
    let mut checker = make_checker(SecurityMode::Standard);
    let result = checker.check_path("write", "/tmp/outside.txt");
    assert!(matches!(result, CheckResult::Ask));
}

// --- YOLO: standard mode fallback for unknown commands ---

#[test]
fn yolo_unknown_bash_is_allowed() {
    // Commands not in default_bash_rules are not matched -> base is None -> YOLO returns Allow
    let mut checker = make_checker(SecurityMode::Yolo);
    assert!(matches!(
        checker.check("bash", "ed somefile"),
        CheckResult::Allowed
    ));
}

#[test]
fn yolo_allows_write_todo_list() {
    let mut checker = make_checker(SecurityMode::Yolo);
    assert!(matches!(
        checker.check("write_todo_list", ""),
        CheckResult::Allowed
    ));
}
