use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use veles_core::{SearchMode, VelesIndex, persist};

use crate::session::{MessageRole, Session, storage};

const SEARCH_LIMIT: usize = 10;
const INDEX_CANDIDATE_LIMIT: usize = SEARCH_LIMIT * 5;

#[derive(Debug, Clone, PartialEq)]
struct SessionSearchHit {
    id: String,
    title: String,
    cwd: String,
    updated_at: String,
    score: f64,
    text: String,
}

#[derive(Debug, Clone)]
struct SessionDocument {
    filename: String,
    id: String,
    title: String,
    cwd: String,
    updated_at: String,
    text: String,
}

pub fn print(query: &str) -> anyhow::Result<()> {
    let query = query.trim();
    if query.is_empty() {
        anyhow::bail!("session search query is empty");
    }
    println!("{}", to_sexp(query, &search(query)?));
    Ok(())
}

fn search(query: &str) -> anyhow::Result<Vec<SessionSearchHit>> {
    let sessions = storage::find_all_sessions()?;
    let documents = session_documents(&sessions);
    if documents.is_empty() {
        return Ok(Vec::new());
    }

    let cache = cache_dir();
    let corpus = cache.join("corpus");
    let index_dir = cache.join("index");
    fs::create_dir_all(&cache)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(cache.join("index.lock"))?;
    lock.lock()?;

    sync_corpus(&corpus, &documents)?;
    let index = load_or_update_index(&corpus, &index_dir)?;
    drop(lock);

    let metadata = documents
        .into_iter()
        .map(|document| (document.filename.clone(), document))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    Ok(index
        .search(
            query,
            INDEX_CANDIDATE_LIMIT,
            SearchMode::Semantic,
            None,
            None,
            None,
        )
        .into_iter()
        .filter_map(|result| {
            let document = metadata.get(&result.chunk.file_path)?;
            seen.insert(document.id.clone()).then(|| SessionSearchHit {
                id: document.id.clone(),
                title: document.title.clone(),
                cwd: document.cwd.clone(),
                updated_at: document.updated_at.clone(),
                score: result.score,
                text: preview(&result.chunk.content),
            })
        })
        .take(SEARCH_LIMIT)
        .collect())
}

fn load_or_update_index(corpus: &Path, index_dir: &Path) -> anyhow::Result<VelesIndex> {
    if persist::index_exists(index_dir) {
        let model = veles_core::model::load_model(None)?;
        match VelesIndex::load(index_dir, model) {
            Ok(mut index) => {
                if !index.update_from_path(corpus)?.is_noop() {
                    index.save(index_dir)?;
                }
                Ok(index)
            }
            Err(_) => build_index(corpus, index_dir),
        }
    } else {
        build_index(corpus, index_dir)
    }
}

fn build_index(corpus: &Path, index_dir: &Path) -> anyhow::Result<VelesIndex> {
    let index = VelesIndex::from_path(corpus, None, None, true)?;
    index.save(index_dir)?;
    Ok(index)
}

fn sync_corpus(corpus: &Path, documents: &[SessionDocument]) -> anyhow::Result<()> {
    fs::create_dir_all(corpus)?;
    let expected = documents
        .iter()
        .map(|document| document.filename.as_str())
        .collect::<HashSet<_>>();
    for entry in fs::read_dir(corpus)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "md")
            && !expected.contains(entry.file_name().to_string_lossy().as_ref())
        {
            fs::remove_file(path)?;
        }
    }
    for document in documents {
        let path = corpus.join(&document.filename);
        if fs::read_to_string(&path).ok().as_deref() != Some(document.text.as_str()) {
            fs::write(path, &document.text)?;
        }
    }
    Ok(())
}

fn session_documents(sessions: &[Session]) -> Vec<SessionDocument> {
    sessions.iter().filter_map(session_document).collect()
}

fn session_document(session: &Session) -> Option<SessionDocument> {
    let mut text = format!(
        "# Session: {}\n# Working directory: {}\n\n",
        session.title(),
        session.working_dir
    );
    for (index, message) in session.messages.iter().enumerate() {
        let role = match message.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            _ => continue,
        };
        if message.content.trim().is_empty() {
            continue;
        }
        text.push_str(&format!(
            "## {role} message {}\n{}\n\n",
            index + 1,
            message.content
        ));
    }
    for compaction in &session.compactions {
        if !compaction.summary.trim().is_empty() {
            text.push_str(&format!(
                "## conversation summary\n{}\n\n",
                compaction.summary
            ));
        }
    }
    (text.lines().count() > 2).then(|| SessionDocument {
        filename: format!("{:x}.md", Sha256::digest(session.id.as_bytes())),
        id: session.id.to_string(),
        title: session.title(),
        cwd: session.working_dir.to_string(),
        updated_at: session.updated_at.to_string(),
        text,
    })
}

fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .map(|directory| directory.join("zerostack"))
        .unwrap_or_else(storage::data_dir)
        .join("session-search")
}

fn preview(text: &str) -> String {
    const LIMIT: usize = 600;
    let text = text.trim();
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let mut preview = text.chars().take(LIMIT).collect::<String>();
    preview.push('…');
    preview
}

fn to_sexp(query: &str, hits: &[SessionSearchHit]) -> String {
    format!(
        "(zerostack-session-search :version 1 :query {} :results ({}))",
        sexp_quote(query),
        hits.iter().map(hit_to_sexp).collect::<Vec<_>>().join(" ")
    )
}

fn hit_to_sexp(hit: &SessionSearchHit) -> String {
    format!(
        "(:id {} :title {} :cwd {} :updated-at {} :score {:.6} :text {})",
        sexp_quote(&hit.id),
        sexp_quote(&hit.title),
        sexp_quote(&hit.cwd),
        sexp_quote(&hit.updated_at),
        hit.score,
        sexp_quote(&hit.text),
    )
}

fn sexp_quote(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    out.push('"');
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push(' '),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_documents_include_conversation_and_summaries() {
        let mut session = Session::new("provider", "model", 1);
        session.id = "session-id".into();
        session.name = "A useful conversation".into();
        session.working_dir = "/work".into();
        session.add_message(MessageRole::User, "Where does authentication happen?");
        session.add_message(
            MessageRole::Assistant,
            "Authentication is resolved in auth.rs.",
        );
        session.add_message(MessageRole::ToolResult, "noisy output");
        session.compactions.push(crate::session::Compaction {
            summary: "Earlier discussion covered OAuth refresh.".into(),
            first_kept_index: 0,
            summarized_count: 0,
            token_savings: 0,
            created_at: "now".into(),
        });

        let documents = session_documents(&[session]);
        assert_eq!(documents.len(), 1);
        assert_eq!(
            documents[0].filename,
            format!("{:x}.md", Sha256::digest(b"session-id"))
        );
        assert!(
            documents[0]
                .text
                .contains("Where does authentication happen?")
        );
        assert!(
            documents[0]
                .text
                .contains("Authentication is resolved in auth.rs.")
        );
        assert!(
            documents[0]
                .text
                .contains("Earlier discussion covered OAuth refresh.")
        );
        assert!(!documents[0].text.contains("noisy output"));
    }

    #[test]
    fn search_sexp_preserves_metadata_and_escapes_text() {
        let sexp = to_sexp(
            "OAuth",
            &[SessionSearchHit {
                id: "session-id".to_string(),
                title: "A \"title\"".to_string(),
                cwd: "/work".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                score: 0.75,
                text: "A matching\nexcerpt".to_string(),
            }],
        );
        assert_eq!(
            sexp,
            "(zerostack-session-search :version 1 :query \"OAuth\" :results ((:id \"session-id\" :title \"A \\\"title\\\"\" :cwd \"/work\" :updated-at \"2026-01-01T00:00:00Z\" :score 0.750000 :text \"A matching\\nexcerpt\")))"
        );
    }
}
