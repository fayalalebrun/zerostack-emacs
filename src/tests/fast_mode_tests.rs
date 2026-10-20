use crate::cli::Cli;
use crate::config::{Config, ServiceTier};
use crate::session::Session;

#[test]
fn fast_mode_startup_precedence_is_cli_then_saved_session_then_config() {
    for cli_tier in [None, Some(ServiceTier::Standard), Some(ServiceTier::Fast)] {
        for saved in [None, Some(ServiceTier::Standard), Some(ServiceTier::Fast)] {
            for configured in [None, Some(ServiceTier::Standard), Some(ServiceTier::Fast)] {
                let cli = Cli {
                    service_tier: cli_tier,
                    ..Default::default()
                };
                let cfg = Config {
                    service_tier: configured,
                    ..Default::default()
                };
                let mut session = Session::new("openai-codex", "gpt-5.5", 1000);
                session.service_tier = saved;
                session.initialize_service_tier(&cli, &cfg);
                assert_eq!(session.service_tier, cli_tier.or(saved).or(configured));
            }
        }
    }
}

#[test]
fn fast_mode_changes_persist_and_resume_without_a_cli_override() {
    let cfg = Config::default();
    let mut session = Session::new("openai-codex", "gpt-5.5", 1000);
    for action in ["on", "off"] {
        assert!(session.update_fast_mode(action, &cfg).unwrap());
        let json = serde_json::to_string(&session).unwrap();
        let mut resumed: Session = serde_json::from_str(&json).unwrap();
        resumed.initialize_service_tier(&Cli::default(), &cfg);
        assert_eq!(resumed.service_tier, session.service_tier);
        assert_eq!(resumed.fast_mode_requested(&cfg), action == "on");
    }
    let mut legacy = serde_json::to_value(&session).unwrap();
    legacy.as_object_mut().unwrap().remove("service_tier");
    let mut resumed: Session = serde_json::from_value(legacy).unwrap();
    resumed.initialize_service_tier(&Cli::default(), &cfg);
    assert_eq!(resumed.service_tier, None);
    assert!(!resumed.fast_mode_requested(&cfg));
}

#[test]
fn fast_mode_status_and_invalid_input_do_not_change_state() {
    let cfg = Config::default();
    let mut session = Session::new("codex", "gpt-5.5", 1000);
    session.update_fast_mode("on", &cfg).unwrap();
    let before = serde_json::to_value(&session).unwrap();
    assert!(!session.update_fast_mode("status", &cfg).unwrap());
    assert!(session.update_fast_mode("invalid", &cfg).is_err());
    assert_eq!(serde_json::to_value(&session).unwrap(), before);
    assert!(
        session
            .fast_mode_message(&cfg)
            .contains("higher subscription credit use")
    );
    assert!(session.update_fast_mode("toggle", &cfg).unwrap());
    assert_eq!(session.service_tier, Some(ServiceTier::Standard));
    session.update_fast_mode("toggle", &cfg).unwrap();
    assert_eq!(session.service_tier, Some(ServiceTier::Fast));
}

#[test]
fn fast_mode_preference_survives_model_and_provider_switches_but_controls_are_codex_only() {
    let cfg = Config::default();
    let mut session = Session::new("openai-codex", "gpt-5.5", 1000);
    session.update_fast_mode("on", &cfg).unwrap();
    session.model = "gpt-5.4".into();
    assert!(session.fast_mode_available());
    assert!(session.fast_mode_requested(&cfg));
    session.provider = "anthropic".into();
    assert!(!session.fast_mode_available());
    assert!(session.update_fast_mode("off", &cfg).is_err());
    assert_eq!(session.service_tier, Some(ServiceTier::Fast));
    session.provider = "codex".into();
    assert!(session.fast_mode_available());
    assert!(session.fast_mode_requested(&cfg));
}

#[test]
fn fast_mode_is_never_inherited_by_child_sessions() {
    let cfg = Config {
        service_tier: Some(ServiceTier::Fast),
        ..Default::default()
    };
    let cli = Cli {
        service_tier: Some(ServiceTier::Fast),
        subagent_parent_session: Some("parent".into()),
        ..Default::default()
    };
    let mut session = Session::new("codex", "gpt-5.5", 1000);
    session.service_tier = Some(ServiceTier::Fast);
    session.initialize_service_tier(&cli, &cfg);
    assert_eq!(session.service_tier, Some(ServiceTier::Standard));
    assert!(!session.fast_mode_requested(&cfg));
}

#[test]
fn fast_mode_controls_can_disable_legacy_extra_body_settings() {
    let cfg = Config {
        extra_body: Some(serde_json::json!({"service_tier": "priority"})),
        ..Default::default()
    };
    let mut session = Session::new("codex", "gpt-5.5", 1000);
    assert!(session.fast_mode_requested(&cfg));
    session.update_fast_mode("toggle", &cfg).unwrap();
    assert!(!session.fast_mode_requested(&cfg));
    assert_eq!(session.service_tier, Some(ServiceTier::Standard));
}

#[test]
fn fast_mode_command_is_available_in_the_tui_picker() {
    let mut picker = crate::ui::pickers::list::ListPicker::with_static_commands();
    picker.activate();
    assert_eq!(
        picker
            .matches
            .iter()
            .filter(|command| command.as_str() == "/fast")
            .count(),
        1
    );
}
