use compact_str::CompactString;

use crate::permission::checker::CheckResult;
use crate::session::Session;

#[test]
fn resumed_session_provider_overrides_current_config_provider() {
    let mut provider = CompactString::new("openai-codex");
    let mut model = CompactString::new("gpt-5.5");
    let session = Session::new("openai", "gpt-5.5", 1_050_000);

    crate::sync_runtime_target_from_session(&mut provider, &mut model, &session);

    assert_eq!(provider.as_str(), "openai");
    assert_eq!(model.as_str(), "gpt-5.5");
}

#[test]
fn server_modes_allow_missing_startup_key() {
    let cli = crate::cli::Cli {
        emacs: true,
        ..Default::default()
    };
    assert!(crate::allow_missing_startup_key(&cli));
}

#[test]
fn presented_skill_files_are_readable_without_permission_requests() {
    let cli = crate::cli::Cli {
        restrictive: true,
        ..Default::default()
    };
    let skills = [
        crate::context::skills::Skill {
            name: "visible-one".to_string(),
            description: "Visible skill".to_string(),
            location: "/outside/one/SKILL.md".into(),
            model_visible: true,
        },
        crate::context::skills::Skill {
            name: "visible-two".to_string(),
            description: "Another visible skill".to_string(),
            location: "/outside/two/SKILL.md".into(),
            model_visible: true,
        },
        crate::context::skills::Skill {
            name: "hidden".to_string(),
            description: "Hidden skill".to_string(),
            location: "/outside/hidden/SKILL.md".into(),
            model_visible: false,
        },
    ];
    let (permission, _, _) =
        crate::build_permission_checker(&cli, &crate::config::Config::default(), &skills);
    let permission = permission.unwrap();
    let mut checker = permission.lock().unwrap();

    for skill in &skills[..2] {
        assert_eq!(
            checker.check_path("read", &skill.location.to_string_lossy()),
            CheckResult::Allowed
        );
    }
    assert_eq!(
        checker.check_path("read", &skills[2].location.to_string_lossy()),
        CheckResult::Ask
    );
}
