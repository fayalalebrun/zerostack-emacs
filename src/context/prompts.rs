use std::collections::HashMap;
use std::path::{Path, PathBuf};

use include_dir::{Dir, include_dir};

static EMBEDDED: Dir = include_dir!("$CARGO_MANIFEST_DIR/data/prompts");

pub fn global_dir() -> PathBuf {
    crate::session::storage::data_dir().join("prompts")
}

pub fn zerostack_dir() -> PathBuf {
    PathBuf::from(".zerostack/prompts")
}

pub fn load() -> HashMap<String, String> {
    load_from(&global_dir(), Path::new("."))
}

fn load_from(global: &Path, project: &Path) -> HashMap<String, String> {
    let mut prompts: HashMap<String, String> = HashMap::new();

    for (name, content) in crate::context::load_embedded_files(&EMBEDDED, "md") {
        prompts.entry(name).or_insert(content);
    }
    for (name, content) in crate::context::load_dir_files(global, "md") {
        prompts.insert(name, content);
    }
    for (name, content) in crate::context::load_dir_files(&project.join("data/prompts"), "md") {
        prompts.insert(name, content);
    }
    for (name, content) in crate::context::load_dir_files(&project.join(zerostack_dir()), "md") {
        prompts.insert(name, content);
    }

    prompts
}

pub fn ensure_global() -> anyhow::Result<()> {
    let dir = global_dir();
    if !dir.exists() {
        crate::context::copy_embedded_to(&EMBEDDED, &dir)?;
    }
    Ok(())
}

pub fn regen() -> anyhow::Result<()> {
    let dir = global_dir();
    crate::context::copy_embedded_to(&EMBEDDED, &dir)
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    struct TestDir {
        dir: PathBuf,
        original_data_dir: Option<PathBuf>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl TestDir {
        fn new() -> Self {
            let lock = TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!("zs_pr_test_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let original_data_dir = crate::session::storage::set_test_data_dir(Some(dir.clone()));
            TestDir {
                dir,
                original_data_dir,
                _lock: lock,
            }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            crate::session::storage::set_test_data_dir(self.original_data_dir.take());
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn load_from(dir: &Path) -> HashMap<String, String> {
        let original = std::env::current_dir().unwrap();
        let prompts = super::load_from(&global_dir(), dir);
        assert_eq!(std::env::current_dir().unwrap(), original);
        prompts
    }

    fn write_prompt(path: &PathBuf, name: &str, content: &str) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join(format!("{}.md", name)), content).unwrap();
    }

    #[test]
    fn test_zerostack_prompts_are_loaded() {
        let td = TestDir::new();
        let dir = td.dir.join(zerostack_dir());
        write_prompt(&dir, "myproject", "# My Project Prompt");

        let prompts = load_from(&td.dir);
        assert!(prompts.contains_key("myproject"));
        assert_eq!(prompts["myproject"], "# My Project Prompt");
    }

    #[test]
    fn test_zerostack_overrides_prompts_dir() {
        let td = TestDir::new();
        let prompts_dir = td.dir.join("prompts");
        let zs_dir = td.dir.join(zerostack_dir());
        write_prompt(&prompts_dir, "code", "from prompts/");
        write_prompt(&zs_dir, "code", "from .zerostack/prompts/");

        let prompts = load_from(&td.dir);
        assert_eq!(prompts["code"], "from .zerostack/prompts/");
    }

    #[test]
    fn test_zerostack_overrides_global() {
        let td = TestDir::new();
        let global = global_dir();
        let zs_dir = td.dir.join(zerostack_dir());
        write_prompt(&global, "code", "from global/");
        write_prompt(&zs_dir, "code", "from .zerostack/");

        let prompts = load_from(&td.dir);
        assert_eq!(prompts["code"], "from .zerostack/");
    }

    #[test]
    fn test_zerostack_overrides_embedded() {
        let td = TestDir::new();
        let zs_dir = td.dir.join(zerostack_dir());
        write_prompt(&zs_dir, "code", "from .zerostack/");

        let prompts = load_from(&td.dir);
        assert_eq!(prompts["code"], "from .zerostack/");
    }

    #[test]
    fn test_prompts_dir_overrides_global() {
        let td = TestDir::new();
        let global = global_dir();
        let prompts_dir = td.dir.join("data/prompts");
        write_prompt(&global, "custom", "from global/");
        write_prompt(&prompts_dir, "custom", "from prompts/");

        let prompts = load_from(&td.dir);
        assert_eq!(prompts["custom"], "from prompts/");
    }

    #[test]
    fn test_full_priority_chain() {
        let td = TestDir::new();
        let global = global_dir();
        let prompts_dir = td.dir.join("data/prompts");
        let zs_dir = td.dir.join(zerostack_dir());

        write_prompt(&global, "code", "from global/");
        write_prompt(&prompts_dir, "custom", "from prompts/");
        write_prompt(&zs_dir, "custom", "from .zerostack/");
        write_prompt(&zs_dir, "code", "from .zerostack/code");

        let prompts = load_from(&td.dir);
        assert_eq!(prompts["code"], "from .zerostack/code");
        assert_eq!(prompts["custom"], "from .zerostack/");
        assert!(prompts.contains_key("ask"));
    }

    #[test]
    fn test_zerostack_dir_missing_is_ok() {
        let td = TestDir::new();
        let prompts = load_from(&td.dir);
        assert!(prompts.contains_key("code"));
        assert!(prompts.contains_key("ask"));
        assert!(prompts.contains_key("default"));
    }
}
