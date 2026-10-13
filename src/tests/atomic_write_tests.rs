//! Tests for `crate::fs::atomic_write`.
//!
//! These exercise the public contract through `atomic_write` only: that writes
//! are atomic (a reader never sees a truncated file), that permissions are
//! preserved, that symlinks are written through rather than clobbered, and that
//! no temp-file residue is left behind.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::fs::atomic_write;

/// A unique temp directory per call, removed on drop. Uniqueness (process id +
/// monotonic counter) keeps parallel test runs from colliding without pulling
/// in an external temp-dir crate.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "zerostack_atomic_test_{}_{}_{}",
            tag,
            std::process::id(),
            n
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Count leftover temp files created by `atomic_write` in a directory.
fn temp_residue(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains(".zswrite."))
                .count()
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn creates_new_file() {
    let dir = TempDir::new("new");
    let f = dir.join("new.txt");
    atomic_write(&f, b"hello world").await.unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), b"hello world");
    assert_eq!(temp_residue(dir.path()), 0);
}

#[tokio::test]
async fn overwrites_existing_file() {
    let dir = TempDir::new("overwrite");
    let f = dir.join("f.txt");
    std::fs::write(&f, b"old contents").unwrap();
    atomic_write(&f, b"new contents").await.unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "new contents");
    assert_eq!(temp_residue(dir.path()), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn preserves_permissions_on_overwrite() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("perms");
    let f = dir.join("script.sh");
    std::fs::write(&f, b"#!/bin/sh\necho old\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();

    atomic_write(&f, b"#!/bin/sh\necho new\n").await.unwrap();

    let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o755,
        "executable bit must survive the atomic replace"
    );
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "#!/bin/sh\necho new\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn writes_through_symlink() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("symlink");
    let real = dir.join("real.txt");
    let link = dir.join("link.txt");
    std::fs::write(&real, b"old").unwrap();
    symlink(&real, &link).unwrap();

    atomic_write(&link, b"new via link").await.unwrap();

    // The link must still be a link, and the real file must hold the new data.
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the symlink itself must be preserved, not replaced with a regular file"
    );
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "new via link");
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "new via link");
    assert_eq!(temp_residue(dir.path()), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn writes_through_symlink_chain() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("chain");
    let real = dir.join("real.txt");
    let link = dir.join("link.txt");
    let link2 = dir.join("link2.txt");
    std::fs::write(&real, b"old").unwrap();
    symlink(&real, &link).unwrap();
    symlink(&link, &link2).unwrap();

    atomic_write(&link2, b"via chain").await.unwrap();

    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::symlink_metadata(&link2)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "via chain");
}

#[cfg(unix)]
#[tokio::test]
async fn resolves_relative_symlink_target() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("relsym");
    let real = dir.join("r.txt");
    let link = dir.join("rlink.txt");
    std::fs::write(&real, b"x").unwrap();
    // Relative target: must resolve against the link's own directory.
    symlink(Path::new("r.txt"), &link).unwrap();

    atomic_write(&link, b"relative ok").await.unwrap();

    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "relative ok");
}

#[tokio::test]
async fn concurrent_writes_to_distinct_files_leave_no_residue() {
    let dir = TempDir::new("concurrent");
    let mut handles = Vec::new();
    for i in 0..50 {
        let p = dir.join(&format!("p{i}.txt"));
        handles.push(tokio::spawn(async move {
            atomic_write(&p, format!("file {i}").into_bytes())
                .await
                .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    for i in 0..50 {
        let p = dir.join(&format!("p{i}.txt"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), format!("file {i}"));
    }
    assert_eq!(temp_residue(dir.path()), 0);
}

/// The core guarantee: while one writer repeatedly replaces a file, a separate
/// OS-thread reader must never observe a truncated or partially-written state —
/// only the complete old value or the complete new value. This is what `rename`
/// buys us and what the old truncate-then-stream write could not.
#[tokio::test]
async fn no_torn_reads_during_rewrites() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let dir = TempDir::new("torn");
    let target = dir.join("hot.txt");
    let size = 64 * 1024;
    atomic_write(&target, vec![b'A'; size]).await.unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let path = target.clone();
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut torn = 0u64;
            while !done.load(Ordering::Relaxed) {
                if let Ok(bytes) = std::fs::read(&path) {
                    let homogeneous =
                        bytes.iter().all(|&c| c == b'A') || bytes.iter().all(|&c| c == b'B');
                    if !(bytes.len() == size && homogeneous) {
                        torn += 1;
                    }
                }
            }
            torn
        })
    };

    for r in 0..300 {
        let fill = if r % 2 == 0 { b'B' } else { b'A' };
        atomic_write(&target, vec![fill; size]).await.unwrap();
    }
    done.store(true, Ordering::Relaxed);

    let torn = reader.join().unwrap();
    assert_eq!(torn, 0, "reader observed {torn} torn/partial states");
    assert_eq!(temp_residue(dir.path()), 0);
}

#[tokio::test]
async fn atomic_create_refuses_existing_entries_and_cleans_temporary_files() {
    let dir = TempDir::new("no_clobber");
    let path = dir.join("file");
    crate::fs::atomic_create(&path, b"first").await.unwrap();
    assert_eq!(
        crate::fs::atomic_create(&path, b"second")
            .await
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    assert_eq!(temp_residue(dir.path()), 0);
    let directory = dir.join("directory");
    std::fs::create_dir(&directory).unwrap();
    assert!(crate::fs::atomic_create(&directory, b"bad").await.is_err());
    assert!(directory.is_dir());
    assert_eq!(temp_residue(dir.path()), 0);
}

#[tokio::test]
async fn file_lock_child_helper() {
    let Some(path) = std::env::var_os("ZEROSTACK_TEST_FILE_LOCK") else {
        return;
    };
    use std::io::Write;
    println!("LOCK_READY");
    std::io::stdout().flush().unwrap();
    let (_, _lock) = crate::fs::lock_file_path(Path::new(&path)).await.unwrap();
    println!("LOCK_ACQUIRED");
    std::io::stdout().flush().unwrap();
}

#[tokio::test]
async fn file_tools_lock_is_shared_between_processes() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let dir = TempDir::new("process_lock");
    let path = dir.join("file");
    std::fs::write(&path, b"source").unwrap();
    let (_, lock) = crate::fs::lock_file_path(&path).await.unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::atomic_write_tests::file_lock_child_helper",
            "--nocapture",
        ])
        .env("ZEROSTACK_TEST_FILE_LOCK", &path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if output
                .next_line()
                .await
                .unwrap()
                .expect("child output")
                .contains("LOCK_READY")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), output.next_line())
            .await
            .is_err()
    );
    drop(lock);
    let line = tokio::time::timeout(std::time::Duration::from_secs(10), output.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(line.contains("LOCK_ACQUIRED"));
    assert!(child.wait().await.unwrap().success());
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_aliases_use_the_same_path_lock() {
    let dir = TempDir::new("alias_lock");
    let target = dir.join("target");
    let alias = dir.join("alias");
    std::fs::write(&target, b"source").unwrap();
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let (first_target, first_lock) = crate::fs::lock_file_path(&target).await.unwrap();
    let second = crate::fs::lock_file_path(&alias);
    tokio::pin!(second);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut second)
            .await
            .is_err()
    );
    drop(first_lock);
    let (second_target, _lock) = tokio::time::timeout(std::time::Duration::from_secs(10), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_target, second_target);
}

#[tokio::test]
async fn atomic_publication_finishes_buffered_writes_before_returning() {
    let dir = TempDir::new("flush_before_publish");
    let existing = dir.join("existing");
    for iteration in 0..20 {
        let payload = vec![b'a' + iteration; 64 * 1024];
        atomic_write(&existing, &payload).await.unwrap();
        // Synchronous reads immediately after the call must see complete bytes.
        assert_eq!(std::fs::read(&existing).unwrap(), payload);
        let new = dir.join(&format!("new-{iteration}"));
        crate::fs::atomic_create(&new, &payload).await.unwrap();
        assert_eq!(std::fs::read(&new).unwrap(), payload);
    }
    assert_eq!(temp_residue(dir.path()), 0);
}
