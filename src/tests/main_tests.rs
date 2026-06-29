use compact_str::CompactString;

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
