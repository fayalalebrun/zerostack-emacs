use crate::agent::tools::{ReadArgs, ReadTool, WriteArgs, WriteTool};
use rig::tool::Tool;

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("zerostack-file-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn read(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<String, crate::agent::tools::ToolError> {
    let output = ReadTool::new(None, None, Some(16), 2000)
        .call(
            &mut rig::tool::ToolContext::new(),
            ReadArgs {
                path: path.into(),
                offset,
                limit,
            },
        )
        .await?
        .into_content();
    let rig::message::ToolResultContent::Text(text) = output.first().unwrap() else {
        panic!("expected text");
    };
    Ok(text.text.clone())
}

#[tokio::test]
async fn failed_and_repeated_reads_are_recoverable_after_external_changes() {
    let dir = TempDir::new();
    let path = dir.path("source.txt");
    assert!(read(&path, None, None).await.is_err());
    std::fs::write(&path, "first\n").unwrap();
    let first = read(&path, None, None).await.unwrap();
    let second = read(&path, None, None).await.unwrap();
    assert!(first.contains("first"));
    assert!(second.contains("first"));
    std::fs::write(&path, "changed externally\n").unwrap();
    assert!(
        read(&path, None, None)
            .await
            .unwrap()
            .contains("changed externally")
    );
    std::fs::remove_file(&path).unwrap();
    assert!(read(&path, None, None).await.is_err());
    std::fs::write(&path, "recreated\n").unwrap();
    assert!(read(&path, None, None).await.unwrap().contains("recreated"));
}

#[tokio::test]
async fn large_file_returns_a_bounded_continuous_window() {
    let dir = TempDir::new();
    let path = dir.path("large.txt");
    let content = (1..=15000)
        .map(|n| format!("line {n} {}\n", "x".repeat(100)))
        .collect::<String>();
    assert!(content.len() > 1024 * 1024);
    std::fs::write(&path, content).unwrap();
    let result = read(&path, Some(100), Some(1000)).await.unwrap();
    assert!(result.len() < 12000);
    assert!(result.contains("line 100 "));
    assert!(!result.contains("tool output truncated"));
    let end: usize = result
        .split("excerpt ends at line ")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(result.contains(&format!("line {end} ")));
    let next = read(&path, Some(end + 1), Some(1)).await.unwrap();
    assert!(next.contains(&format!("line {} ", end + 1)));
}

#[tokio::test]
async fn zero_read_arguments_are_rejected() {
    let dir = TempDir::new();
    let path = dir.path("text");
    std::fs::write(&path, "valid").unwrap();
    assert!(read(&path, Some(0), None).await.is_err());
    assert!(read(&path, None, Some(0)).await.is_err());
}

#[tokio::test]
async fn concurrent_creation_publishes_only_one_complete_file() {
    let dir = TempDir::new();
    let path = dir.path("new.txt");
    let tool = WriteTool::new(None, None, None);
    let mut left_ctx = rig::tool::ToolContext::new();
    let mut right_ctx = rig::tool::ToolContext::new();
    let left = tool.call(
        &mut left_ctx,
        WriteArgs {
            path: path.clone(),
            content: "left".into(),
        },
    );
    let right = tool.call(
        &mut right_ctx,
        WriteArgs {
            path: path.clone(),
            content: "right".into(),
        },
    );
    let (left, right) = tokio::join!(left, right);
    assert_ne!(left.is_ok(), right.is_ok());
    assert!(left.as_ref().or(right.as_ref()).unwrap().contains("[CRC:"));
    let result = std::fs::read_to_string(&path).unwrap();
    assert!(result == "left" || result == "right");
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[tokio::test]
async fn oversize_creation_does_not_create_parent_directories() {
    let dir = TempDir::new();
    let path = dir.path("missing/subdir/file");
    let tool = WriteTool::new(None, None, Some(1));
    assert!(
        tool.call(
            &mut rig::tool::ToolContext::new(),
            WriteArgs {
                path,
                content: "too large".into()
            }
        )
        .await
        .is_err()
    );
    assert!(!dir.0.join("missing").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn creation_refuses_live_and_dangling_symlinks() {
    let dir = TempDir::new();
    let target = dir.path("target");
    let link = dir.path("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let tool = WriteTool::new(None, None, None);
    let args = || WriteArgs {
        path: link.clone(),
        content: "bad".into(),
    };
    assert!(
        tool.call(&mut rig::tool::ToolContext::new(), args())
            .await
            .is_err()
    );
    assert!(!std::path::Path::new(&target).exists());
    std::fs::write(&target, "keep").unwrap();
    assert!(
        tool.call(&mut rig::tool::ToolContext::new(), args())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(feature = "multimodal")]
#[tokio::test]
async fn extensionless_images_still_return_image_content() {
    let dir = TempDir::new();
    let path = dir.path("no-extension");
    std::fs::write(&path, crate::extras::image_validate::test_png()).unwrap();
    let content = ReadTool::new(None, None, None, 2000)
        .call(
            &mut rig::tool::ToolContext::new(),
            ReadArgs {
                path,
                offset: None,
                limit: None,
            },
        )
        .await
        .unwrap()
        .into_content();
    assert!(matches!(
        content.iter().nth(1),
        Some(rig::message::ToolResultContent::Image(_))
    ));
}
