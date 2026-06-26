use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "needs_attention.json";

pub fn mark(session_id: &str) -> anyhow::Result<()> {
    update(|ids| {
        ids.insert(session_id.to_string());
    })
}

pub fn dismiss(session_id: &str) -> anyhow::Result<()> {
    update(|ids| {
        ids.remove(session_id);
    })
}

pub fn list() -> anyhow::Result<BTreeSet<String>> {
    read_ids(&attention_path())
}

fn update(mut f: impl FnMut(&mut BTreeSet<String>)) -> anyhow::Result<()> {
    let path = attention_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut ids = read_ids(&path)?;
    f(&mut ids);
    write_ids(&path, &ids)
}

fn read_ids(path: &Path) -> anyhow::Result<BTreeSet<String>> {
    if !path.exists() {
        return Ok(BTreeSet::new());
    }
    let json = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&json).unwrap_or_default())
}

fn write_ids(path: &Path, ids: &BTreeSet<String>) -> anyhow::Result<()> {
    let temp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&temp, serde_json::to_string_pretty(ids)?)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

fn attention_path() -> PathBuf {
    runtime_root().join(FILE_NAME)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_ids_tolerates_missing_and_invalid_file() {
        let root = std::env::temp_dir().join(format!("zs-attention-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("attention.json");

        assert!(read_ids(&path).unwrap().is_empty());
        std::fs::write(&path, "not json").unwrap();
        assert!(read_ids(&path).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_and_read_ids_round_trip() {
        let root =
            std::env::temp_dir().join(format!("zs-attention-roundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("attention.json");
        let ids = BTreeSet::from(["a".to_string(), "b".to_string()]);

        write_ids(&path, &ids).unwrap();

        assert_eq!(read_ids(&path).unwrap(), ids);
        let _ = std::fs::remove_dir_all(&root);
    }
}
