use std::path::{Path, PathBuf};
use std::process::Command;

use veles_core::{Chunk, SearchMode, SearchResult};

use crate::agent::tools::veles::{
    cache_host_for, format_results, repository_paths, search_repo, search_scoped,
};
use crate::permission::checker::{CheckResult, PermissionChecker};
use crate::permission::{PermissionConfigs, SecurityMode};

#[test]
fn prompt_routes_unknown_locations_to_code_search() {
    assert!(crate::agent::prompt::CODE_SEARCH_PROMPT.contains("Use `code_search` first"));
    assert!(crate::agent::prompt::CODE_SEARCH_PROMPT.contains("Use `grep` for exact"));
    assert!(!crate::agent::prompt::SYSTEM_PROMPT.contains("Prefer grep"));
}

#[test]
fn cache_path_is_stable_and_outside_repository() {
    let base = Path::new("/cache/zerostack/veles");
    let first = cache_host_for(Path::new("/repo/.git"), base);
    let second = cache_host_for(Path::new("/repo/.git"), base);
    let other = cache_host_for(Path::new("/other/.git"), base);

    assert_eq!(first, second);
    assert_ne!(first, other);
    assert_eq!(first.parent(), Some(base));
    assert!(!first.starts_with("/repo"));
}

#[test]
fn formats_search_results_for_agent_context() {
    let results = vec![SearchResult {
        chunk: Chunk {
            content: "fn search() {}".to_string(),
            file_path: "src/search.rs".to_string(),
            start_line: 10,
            end_line: 12,
            language: Some("rust".to_string()),
        },
        score: 0.5,
        source: SearchMode::Hybrid,
    }];

    let output = format_results(&results);
    assert!(output.contains("src/search.rs:10-12 [0.5000, hybrid]"));
    assert!(output.contains("```rust\nfn search() {}\n```"));
}

#[test]
fn formats_empty_search_results() {
    assert_eq!(format_results(&[]), "No matches found.");
}

#[test]
fn code_search_is_allowed_as_a_read_tool() {
    let mut checker = PermissionChecker::new(
        &PermissionConfigs::default(),
        SecurityMode::ReadOnly,
        Some("/repo".into()),
        None,
    );
    assert!(matches!(
        checker.check("code_search", "find parser"),
        CheckResult::Allowed
    ));
}

#[test]
fn code_search_path_permissions_distinguish_external_paths() {
    let mut checker = PermissionChecker::new(
        &PermissionConfigs::default(),
        SecurityMode::Standard,
        Some("/repo".into()),
        None,
    );
    assert!(matches!(
        checker.check_path("code_search", "src"),
        CheckResult::Allowed
    ));
    assert!(matches!(
        checker.check_path("code_search", "/other/repo"),
        CheckResult::Ask
    ));
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("zs-veles-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn linked_worktrees_resolve_to_same_cache_identity() {
    let temp = TestDir::new("worktree");
    let repo = temp.0.join("repo");
    let worktree = temp.0.join("linked");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "tests@example.com"]);
    git(&repo, &["config", "user.name", "Tests"]);
    std::fs::write(repo.join("lib.rs"), "fn baseline() {}\n").unwrap();
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-m", "baseline"]);
    git(
        &repo,
        &[
            "worktree",
            "add",
            worktree.to_str().unwrap(),
            "-b",
            "linked",
        ],
    );

    let (_, repo_identity) = repository_paths(&repo).unwrap();
    let (_, worktree_identity) = repository_paths(&worktree).unwrap();
    assert_eq!(repo_identity, worktree_identity);
    assert_eq!(
        cache_host_for(&repo_identity, Path::new("/cache")),
        cache_host_for(&worktree_identity, Path::new("/cache"))
    );
}

#[test]
#[ignore = "downloads the embedding model"]
fn real_index_update_and_search_roundtrip() {
    let temp = TestDir::new("roundtrip");
    let repo = temp.0.join("repo");
    let cache = temp.0.join("cache");
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(repo.join("focused")).unwrap();
    std::fs::create_dir(repo.join("noise")).unwrap();
    std::fs::write(
        repo.join("focused/parser.rs"),
        "fn parse_constellation_manifest() { let nebula_token = 42; }\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("noise/parser.rs"),
        "fn unrelated_constellation_parser() {}\n",
    )
    .unwrap();

    let first = search_repo(
        &repo,
        &cache,
        "constellation manifest parser",
        5,
        SearchMode::Hybrid,
        None,
    )
    .unwrap();
    assert!(first.contains("parser.rs"), "{first}");

    std::fs::write(
        repo.join("focused/parser.rs"),
        "fn parse_galactic_manifest() { let quasar_token = 42; }\n",
    )
    .unwrap();
    let second = search_repo(
        &repo,
        &cache,
        "galactic manifest quasar",
        5,
        SearchMode::Hybrid,
        None,
    )
    .unwrap();
    assert!(second.contains("parse_galactic_manifest"), "{second}");
    let scoped = search_scoped(
        &repo,
        Some("focused"),
        &cache,
        "constellation parser",
        5,
        SearchMode::Hybrid,
        None,
    )
    .unwrap();
    assert!(scoped.contains("focused/parser.rs"), "{scoped}");
    assert!(!scoped.contains("noise/parser.rs"), "{scoped}");
    let absolute = search_scoped(
        &repo,
        Some(repo.join("focused").to_str().unwrap()),
        &cache,
        "galactic manifest",
        5,
        SearchMode::Hybrid,
        Some(&["rust".to_string()]),
    )
    .unwrap();
    assert!(absolute.contains("focused/parser.rs"), "{absolute}");
    assert!(!absolute.contains("noise/parser.rs"), "{absolute}");
    assert!(
        cache_host_for(&repo.canonicalize().unwrap(), &cache)
            .join(".veles/manifest.json")
            .is_file()
    );

    let executable = std::env::current_exe().unwrap();
    let spawn_search = |query: &str| {
        Command::new(&executable)
            .args([
                "--ignored",
                "--exact",
                "tests::veles_tests::veles_subprocess_search_helper",
            ])
            .env("ZS_VELES_TEST_REPO", &repo)
            .env("ZS_VELES_TEST_CACHE", &cache)
            .env("ZS_VELES_TEST_QUERY", query)
            .spawn()
            .unwrap()
    };
    let mut first = spawn_search("galactic manifest");
    let mut second = spawn_search("quasar token");
    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());

    let cache_host = cache_host_for(&repo.canonicalize().unwrap(), &cache);
    std::fs::write(cache_host.join(".veles/chunks.bin"), b"corrupt").unwrap();
    let recovered = search_repo(
        &repo,
        &cache,
        "galactic manifest quasar",
        5,
        SearchMode::Hybrid,
        None,
    )
    .unwrap();
    assert!(recovered.contains("parse_galactic_manifest"), "{recovered}");
}

#[test]
#[ignore = "subprocess helper"]
fn veles_subprocess_search_helper() {
    let Some(repo) = std::env::var_os("ZS_VELES_TEST_REPO") else {
        return;
    };
    let cache = std::env::var_os("ZS_VELES_TEST_CACHE").unwrap();
    let query = std::env::var("ZS_VELES_TEST_QUERY").unwrap();
    let output = search_repo(
        Path::new(&repo),
        Path::new(&cache),
        &query,
        5,
        SearchMode::Hybrid,
        None,
    )
    .unwrap();
    assert!(output.contains("parser.rs"), "{output}");
}

#[test]
#[ignore = "indexes the full repository"]
fn real_current_repository_search_smoke() {
    let cache = TestDir::new("current-repo-cache");
    let output = search_repo(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &cache.0,
        "permission checker repeated tool call doom loop",
        10,
        SearchMode::Hybrid,
        None,
    )
    .unwrap();
    assert!(output.contains("permission/checker.rs"), "{output}");
}

#[test]
#[ignore = "uses the embedding model"]
fn real_linked_worktrees_share_and_refresh_one_index() {
    let temp = TestDir::new("worktree-roundtrip");
    let repo = temp.0.join("repo");
    let worktree = temp.0.join("linked");
    let cache = temp.0.join("cache");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "tests@example.com"]);
    git(&repo, &["config", "user.name", "Tests"]);
    std::fs::write(repo.join("engine.rs"), "fn baseline_engine() {}\n").unwrap();
    git(&repo, &["add", "engine.rs"]);
    git(&repo, &["commit", "-m", "baseline"]);
    git(
        &repo,
        &[
            "worktree",
            "add",
            worktree.to_str().unwrap(),
            "-b",
            "linked",
        ],
    );

    let baseline = search_repo(
        &repo,
        &cache,
        "baseline engine",
        5,
        SearchMode::Hybrid,
        None,
    )
    .unwrap();
    assert!(baseline.contains("baseline_engine"), "{baseline}");

    std::fs::write(
        worktree.join("engine.rs"),
        "fn worktree_quantum_engine() {}\n",
    )
    .unwrap();
    let refreshed = search_repo(
        &worktree,
        &cache,
        "worktree quantum engine",
        5,
        SearchMode::Hybrid,
        None,
    )
    .unwrap();
    assert!(refreshed.contains("worktree_quantum_engine"), "{refreshed}");
    assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
}
