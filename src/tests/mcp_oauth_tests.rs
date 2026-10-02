use crate::extras::mcp::config::{
    DEFAULT_REDIRECT_PORT, McpServerConfig, OAuthConfig, OAuthSettings,
};
use crate::extras::mcp::oauth;

#[test]
fn url_server_without_oauth_parses() {
    let json = r#"{ "url": "https://example.com/mcp" }"#;
    let cfg: McpServerConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.connect_timeout().unwrap().as_secs(), 30);
    assert_eq!(cfg.discovery_timeout().unwrap().as_secs(), 30);
    assert_eq!(cfg.tool_timeout().unwrap().as_secs(), 300);
    match cfg {
        McpServerConfig::Url { url, oauth, .. } => {
            assert_eq!(url, "https://example.com/mcp");
            assert!(oauth.is_none());
        }
        _ => panic!("expected Url variant"),
    }
}

#[test]
fn zero_disables_mcp_timeouts() {
    let json = r#"{
        "command": "server",
        "connect_timeout_secs": 0,
        "discovery_timeout_secs": 0,
        "tool_timeout_secs": 0
    }"#;
    let cfg: McpServerConfig = serde_json::from_str(json).unwrap();
    assert!(cfg.connect_timeout().is_none());
    assert!(cfg.discovery_timeout().is_none());
    assert!(cfg.tool_timeout().is_none());
}

#[test]
fn oauth_true_enables_with_defaults() {
    let json = r#"{ "url": "https://example.com/mcp", "oauth": true }"#;
    let cfg: McpServerConfig = serde_json::from_str(json).unwrap();
    let McpServerConfig::Url { oauth, .. } = cfg else {
        panic!("expected Url variant");
    };
    let settings = oauth.unwrap().settings().expect("oauth enabled");
    assert!(settings.scopes.is_empty());
    assert!(settings.client_id.is_none());
    assert_eq!(settings.redirect_port(), DEFAULT_REDIRECT_PORT);
}

#[test]
fn oauth_false_disables() {
    let json = r#"{ "url": "https://example.com/mcp", "oauth": false }"#;
    let cfg: McpServerConfig = serde_json::from_str(json).unwrap();
    let McpServerConfig::Url { oauth, .. } = cfg else {
        panic!("expected Url variant");
    };
    assert!(oauth.unwrap().settings().is_none());
}

#[test]
fn oauth_object_parses_fields() {
    let json = r#"{
        "url": "https://example.com/mcp",
        "oauth": { "scopes": ["read", "write"], "client_id": "abc", "redirect_port": 9123 }
    }"#;
    let cfg: McpServerConfig = serde_json::from_str(json).unwrap();
    let McpServerConfig::Url { oauth, .. } = cfg else {
        panic!("expected Url variant");
    };
    let settings = oauth.unwrap().settings().unwrap();
    assert_eq!(
        settings.scopes,
        vec!["read".to_string(), "write".to_string()]
    );
    assert_eq!(settings.client_id.as_deref(), Some("abc"));
    assert_eq!(settings.redirect_port(), 9123);
    assert_eq!(settings.redirect_uri(), "http://127.0.0.1:9123/callback");
}

#[test]
fn default_redirect_uri_uses_loopback() {
    let settings = OAuthSettings::default();
    assert_eq!(
        settings.redirect_uri(),
        format!("http://127.0.0.1:{DEFAULT_REDIRECT_PORT}/callback")
    );
}

#[test]
fn token_filename_sanitizes_server_name() {
    assert_eq!(
        oauth::token_filename("Exa Web Search"),
        "Exa_Web_Search.json"
    );
    assert_eq!(oauth::token_filename("../etc/passwd"), "___etc_passwd.json");
    assert_eq!(oauth::token_filename("plain-name_1"), "plain-name_1.json");
}

#[test]
fn parse_callback_extracts_code_and_state() {
    let line = "GET /callback?code=abc123&state=xyz789 HTTP/1.1";
    let (code, state, _) = oauth::parse_callback(line).unwrap();
    assert_eq!(code, "abc123");
    assert_eq!(state, "xyz789");
}

#[test]
fn parse_callback_decodes_percent_escapes() {
    let line = "GET /callback?code=a%2Fb%2Bc&state=s%20t HTTP/1.1";
    let (code, state, _) = oauth::parse_callback(line).unwrap();
    assert_eq!(code, "a/b+c");
    assert_eq!(state, "s t");
}

#[test]
fn parse_callback_preserves_oauth_issuer() {
    let line = "GET /callback?code=abc&state=xyz&iss=https%3A%2F%2Fmcp.linear.app HTTP/1.1";
    let (code, state, issuer) = oauth::parse_callback(line).unwrap();
    assert_eq!(code, "abc");
    assert_eq!(state, "xyz");
    assert_eq!(issuer.as_deref(), Some("https://mcp.linear.app"));
    let (_, _, issuer) =
        oauth::parse_callback("GET /callback?code=abc&state=xyz HTTP/1.1").unwrap();
    assert!(issuer.is_none());
}

#[test]
fn parse_callback_reports_server_error() {
    let line = "GET /callback?error=access_denied HTTP/1.1";
    assert!(oauth::parse_callback(line).is_err());
}

#[test]
fn parse_callback_missing_params_errors() {
    let line = "GET /callback?code=only HTTP/1.1";
    assert!(oauth::parse_callback(line).is_err());
}

#[test]
fn percent_decode_handles_plus_and_hex() {
    assert_eq!(oauth::percent_decode("a+b"), "a b");
    assert_eq!(oauth::percent_decode("%41%42"), "AB");
    assert_eq!(oauth::percent_decode("nochange"), "nochange");
    // Malformed escape is left as-is.
    assert_eq!(oauth::percent_decode("%zz"), "%zz");
}

#[test]
fn oauth_config_round_trips_through_serde() {
    let cfg = McpServerConfig::Url {
        url: "https://example.com/mcp".to_string(),
        headers: Default::default(),
        oauth: Some(OAuthConfig::Enabled(true)),
        connect_timeout_secs: 10,
        discovery_timeout_secs: 20,
        tool_timeout_secs: 0,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: McpServerConfig = serde_json::from_str(&json).unwrap();
    let McpServerConfig::Url {
        oauth,
        connect_timeout_secs,
        discovery_timeout_secs,
        tool_timeout_secs,
        ..
    } = back
    else {
        panic!("expected Url variant");
    };
    assert!(oauth.unwrap().settings().is_some());
    assert_eq!(connect_timeout_secs, 10);
    assert_eq!(discovery_timeout_secs, 20);
    assert_eq!(tool_timeout_secs, 0);
}
