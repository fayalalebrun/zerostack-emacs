use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::session::storage;

const HOME_SKILL_DIRS: &[&[&str]] = &[
    &[".config", "opencode", "skills"],
    &[".opencode", "skills"],
    &[".claude", "skills"],
    &[".pi", "agent", "skills"],
    &[".agents", "skills"],
];

const PROJECT_SKILL_DIRS: &[&[&str]] = &[
    &[".opencode", "skills"],
    &[".claude", "skills"],
    &[".pi", "skills"],
    &[".agents", "skills"],
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub location: PathBuf,
    pub model_visible: bool,
}

pub fn load() -> Vec<Skill> {
    let mut skills = Vec::new();
    let mut seen = HashSet::new();
    for dir in skill_dirs() {
        load_from_dir(&dir, &mut seen, &mut skills);
    }
    skills
}

pub fn format_for_prompt(skills: &[Skill]) -> Option<String> {
    let visible = skills
        .iter()
        .filter(|skill| skill.model_visible)
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return None;
    }

    let mut out = String::from(
        "The following skills provide specialized instructions for specific tasks.\n\
Use the read tool to load a skill's file when the task matches its description.\n\
When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md) and use that absolute path in tool commands.\n\n\
<available_skills>\n",
    );
    for skill in visible {
        out.push_str("  <skill>\n");
        out.push_str("    <name>");
        out.push_str(&xml_escape(&skill.name));
        out.push_str("</name>\n");
        out.push_str("    <description>");
        out.push_str(&xml_escape(&skill.description));
        out.push_str("</description>\n");
        out.push_str("    <location>");
        out.push_str(&xml_escape(&skill.location.display().to_string()));
        out.push_str("</location>\n");
        out.push_str("  </skill>\n");
    }
    out.push_str("</available_skills>");
    Some(out)
}

fn skill_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home_dir() {
        for parts in HOME_SKILL_DIRS {
            dirs.push(join_parts(&home, parts));
        }
    }
    dirs.push(storage::config_path().join("agent").join("skills"));

    if let Ok(cwd) = std::env::current_dir() {
        let root = git_root(&cwd).unwrap_or(cwd.clone());
        let mut current = Some(cwd.as_path());
        while let Some(dir) = current {
            for parts in PROJECT_SKILL_DIRS {
                dirs.push(join_parts(dir, parts));
            }
            if dir == root {
                break;
            }
            current = dir.parent();
        }
    }

    let mut seen = HashSet::new();
    dirs.into_iter()
        .filter(|dir| seen.insert(dir.clone()))
        .collect()
}

fn join_parts(base: &Path, parts: &[&str]) -> PathBuf {
    parts
        .iter()
        .fold(base.to_path_buf(), |path, part| path.join(part))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn load_from_dir(dir: &Path, seen: &mut HashSet<String>, out: &mut Vec<Skill>) {
    if !dir.is_dir() {
        return;
    }
    visit_dir(dir, seen, out);
}

fn visit_dir(dir: &Path, seen: &mut HashSet<String>, out: &mut Vec<Skill>) {
    let skill_file = dir.join("SKILL.md");
    if skill_file.is_file() {
        if let Some(skill) = load_from_file(&skill_file)
            && seen.insert(skill.name.clone())
        {
            out.push(skill);
        }
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            visit_dir(&path, seen, out);
        }
    }
}

fn load_from_file(path: &Path) -> Option<Skill> {
    let content = std::fs::read_to_string(path).ok()?;
    let frontmatter = frontmatter(&content);
    let default_name = path.parent()?.file_name()?.to_str()?.to_string();
    let name = frontmatter
        .get("name")
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or(default_name);
    let description = frontmatter.get("description")?.trim().to_string();
    if description.is_empty() {
        return None;
    }
    let model_visible = frontmatter
        .get("disable-model-invocation")
        .map(|value| !matches!(value.trim(), "true" | "yes" | "1"))
        .unwrap_or(true);
    let location = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Some(Skill {
        name,
        description,
        location,
        model_visible,
    })
}

fn frontmatter(content: &str) -> std::collections::HashMap<String, String> {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return std::collections::HashMap::new();
    }

    let mut values = std::collections::HashMap::new();
    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            values.insert(key.trim().to_string(), unquote(value.trim()).to_string());
        }
    }
    values
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("zs_skills_test_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_skill(dir: &Path, frontmatter: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(&path, format!("---\n{frontmatter}\n---\nBody\n")).unwrap();
        path
    }

    #[test]
    fn discovers_skill_files_recursively_and_dedupes_names() {
        let root = temp_root("discovery");
        write_skill(
            &root.join("alpha"),
            "name: shared\ndescription: First description",
        );
        write_skill(
            &root.join("nested").join("beta"),
            "name: beta\ndescription: Nested skill",
        );
        write_skill(
            &root.join("duplicate"),
            "name: shared\ndescription: Should lose",
        );
        write_skill(
            &root.join(".hidden").join("hidden"),
            "name: hidden\ndescription: Hidden",
        );
        write_skill(
            &root.join("node_modules").join("ignored"),
            "name: ignored\ndescription: Ignored",
        );

        let mut seen = HashSet::new();
        let mut skills = Vec::new();
        load_from_dir(&root, &mut seen, &mut skills);

        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "shared");
        assert_eq!(skills[0].description, "First description");
        assert_eq!(skills[1].name, "beta");
        assert!(!skills.iter().any(|skill| skill.name == "hidden"));
        assert!(!skills.iter().any(|skill| skill.name == "ignored"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn skips_missing_description_and_honors_disable_model_invocation() {
        let root = temp_root("frontmatter");
        let missing = write_skill(&root.join("missing"), "name: missing");
        assert!(load_from_file(&missing).is_none());

        let disabled = write_skill(
            &root.join("disabled"),
            "name: disabled\ndescription: Hidden from prompt\ndisable-model-invocation: true",
        );
        let skill = load_from_file(&disabled).unwrap();
        assert_eq!(skill.name, "disabled");
        assert!(!skill.model_visible);
        assert!(format_for_prompt(&[skill]).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn format_escapes_skill_metadata() {
        let skills = vec![Skill {
            name: "a<b".to_string(),
            description: "use & check".to_string(),
            location: PathBuf::from("/tmp/SKILL.md"),
            model_visible: true,
        }];
        let prompt = format_for_prompt(&skills).unwrap();
        assert!(prompt.contains("a&lt;b"));
        assert!(prompt.contains("use &amp; check"));
    }

    #[test]
    fn includes_claude_skill_directories() {
        assert!(
            HOME_SKILL_DIRS
                .iter()
                .any(|parts| *parts == [".claude", "skills"])
        );
        assert!(
            PROJECT_SKILL_DIRS
                .iter()
                .any(|parts| *parts == [".claude", "skills"])
        );

        let home = PathBuf::from("/home/example");
        let project = PathBuf::from("/repo/example");
        assert_eq!(
            join_parts(&home, &[".claude", "skills"]),
            PathBuf::from("/home/example/.claude/skills")
        );
        assert_eq!(
            join_parts(&project, &[".claude", "skills"]),
            PathBuf::from("/repo/example/.claude/skills")
        );
    }
}
