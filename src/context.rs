use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_IGNORE_PATTERNS: &[&str] = &[
    ".git",
    ".omni",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "dist",
    "build",
    "__pycache__",
    ".idea",
    ".vscode",
];

const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const PREVIEW_LINES: usize = 24;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectProfile {
    pub languages: Vec<String>,
    pub package_managers: Vec<String>,
    pub frameworks: Vec<String>,
    pub total_files: usize,
    pub indexed_files: usize,
    pub total_bytes: u64,
    pub estimated_tokens: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub language: Option<String>,
    pub bytes: u64,
    pub estimated_tokens: usize,
    pub preview: String,
    pub is_ignored: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub path: String,
    pub score: f64,
    pub explanation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextEngine {
    pub workspace: PathBuf,
    pub profile: ProjectProfile,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Error)]
pub enum ContextError {
    #[error("workspace path is not a directory: {0}")]
    NotADirectory(PathBuf),
    #[error("path traversal detected: {0}")]
    PathEscape(PathBuf),
    #[error("I/O error while indexing: {0}")]
    Io(#[from] std::io::Error),
}

impl ContextEngine {
    pub fn index(workspace: PathBuf) -> Result<Self, ContextError> {
        let canonical = fs::canonicalize(&workspace)?;
        if !canonical.is_dir() {
            return Err(ContextError::NotADirectory(canonical));
        }

        let ignore = load_ignore_rules(&canonical);
        let mut files = Vec::new();
        let mut languages = HashSet::new();
        let mut package_managers = HashSet::new();
        let mut frameworks = HashSet::new();
        let mut total_bytes: u64 = 0;
        let mut indexed = 0usize;

        visit(&canonical, &canonical, &ignore, &mut |path, metadata| {
            let relative = strip_prefix(path, &canonical);
            let language = classify_language(path);
            let (preview, tokens) = if metadata.len() <= MAX_FILE_BYTES {
                match fs::read_to_string(path) {
                    Ok(text) => (preview_text(&text), estimate_tokens(text.len())),
                    Err(_) => (String::new(), estimate_tokens(metadata.len() as usize)),
                }
            } else {
                (String::new(), estimate_tokens(MAX_FILE_BYTES as usize))
            };

            if let Some(lang) = language {
                languages.insert(lang.to_string());
            }
            detect_stack(path, &mut package_managers, &mut frameworks);

            total_bytes += metadata.len();
            if !relative.is_empty() {
                indexed += 1;
            }

            files.push(FileEntry {
                path: relative,
                language: language.map(String::from),
                bytes: metadata.len(),
                estimated_tokens: tokens,
                preview,
                is_ignored: false,
            });

            if total_bytes > MAX_INDEX_BYTES {
                return false;
            }
            true
        })?;

        let mut language_vec: Vec<String> = languages.into_iter().collect();
        language_vec.sort();
        let mut pm_vec: Vec<String> = package_managers.into_iter().collect();
        pm_vec.sort();
        let mut fw_vec: Vec<String> = frameworks.into_iter().collect();
        fw_vec.sort();

        files.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(Self {
            workspace: canonical.clone(),
            profile: ProjectProfile {
                languages: language_vec,
                package_managers: pm_vec,
                frameworks: fw_vec,
                total_files: files.len(),
                indexed_files: indexed,
                total_bytes,
                estimated_tokens: estimate_tokens(total_bytes as usize),
            },
            files,
        })
    }

    pub fn query(&self, prompt: &str, top_k: usize) -> Vec<SearchResult> {
        let terms = tokenize(prompt);
        if terms.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<SearchResult> = self
            .files
            .iter()
            .filter(|entry| !entry.is_ignored)
            .map(|entry| {
                let (score, matched) = score_entry(entry, &terms);
                let explanation = if matched.is_empty() {
                    "path match".into()
                } else {
                    format!("matched keywords: {}", matched.join(", "))
                };
                (entry, score, explanation)
            })
            .filter(|(_, score, _)| *score > 0.0)
            .map(|(entry, score, explanation)| SearchResult {
                path: entry.path.clone(),
                score,
                explanation,
            })
            .collect();

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        scored.truncate(top_k);
        scored
    }
}

fn visit(
    root: &Path,
    current: &Path,
    ignore: &[String],
    callback: &mut dyn FnMut(&Path, &fs::Metadata) -> bool,
) -> Result<(), ContextError> {
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let relative = strip_prefix(&path, root);

        if is_ignored_path(&name, &relative, ignore) {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        if metadata.is_dir() {
            visit(root, &path, ignore, callback)?;
        } else if metadata.is_file() && !callback(&path, &metadata) {
            return Ok(());
        }
    }

    Ok(())
}

fn load_ignore_rules(workspace: &Path) -> Vec<String> {
    let mut rules: Vec<String> = DEFAULT_IGNORE_PATTERNS
        .iter()
        .map(|s| s.to_string())
        .collect();
    for filename in [".gitignore", ".omniignore"] {
        if let Ok(text) = fs::read_to_string(workspace.join(filename)) {
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                rules.push(trimmed.to_string());
            }
        }
    }
    rules
}

fn is_ignored_path(name: &str, relative: &str, rules: &[String]) -> bool {
    for rule in rules {
        if rule == name {
            return true;
        }
        if rule.ends_with('/') && relative.starts_with(rule.trim_end_matches('/')) {
            return true;
        }
        if glob_match(rule, name) {
            return true;
        }
    }
    false
}

fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern.contains('/') {
        return false;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut cursor = name;
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if index == 0 && !cursor.starts_with(part) {
            return false;
        }
        if let Some(position) = cursor.find(part) {
            cursor = &cursor[position + part.len()..];
        } else {
            return false;
        }
    }
    true
}

fn strip_prefix(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn classify_language(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => Some("rust"),
        Some("py") => Some("python"),
        Some("js") => Some("javascript"),
        Some("ts") => Some("typescript"),
        Some("jsx") => Some("jsx"),
        Some("tsx") => Some("tsx"),
        Some("go") => Some("go"),
        Some("c") | Some("h") => Some("c"),
        Some("cpp") | Some("cc") | Some("hpp") => Some("cpp"),
        Some("java") => Some("java"),
        Some("kt") => Some("kotlin"),
        Some("swift") => Some("swift"),
        Some("rb") => Some("ruby"),
        Some("php") => Some("php"),
        Some("cs") => Some("csharp"),
        Some("fs") => Some("fsharp"),
        Some("scala") => Some("scala"),
        Some("r") => Some("r"),
        Some("m") => Some("objectivec"),
        Some("mm") => Some("objectivecpp"),
        Some("sh") => Some("shell"),
        Some("ps1") => Some("powershell"),
        Some("yaml") | Some("yml") => Some("yaml"),
        Some("toml") => Some("toml"),
        Some("json") => Some("json"),
        Some("md") => Some("markdown"),
        Some("html") => Some("html"),
        Some("css") => Some("css"),
        Some("scss") | Some("sass") => Some("scss"),
        Some("sql") => Some("sql"),
        _ => None,
    }
}

fn detect_stack(
    path: &Path,
    package_managers: &mut HashSet<String>,
    frameworks: &mut HashSet<String>,
) {
    match path.file_name().and_then(|name| name.to_str()) {
        Some("Cargo.toml") => {
            package_managers.insert("cargo".into());
            if let Ok(text) = fs::read_to_string(path) {
                if text.contains("tokio") {
                    frameworks.insert("tokio".into());
                }
                if text.contains("axum") {
                    frameworks.insert("axum".into());
                }
            }
        }
        Some("package.json") => {
            package_managers.insert("npm".into());
            if let Ok(text) = fs::read_to_string(path) {
                if text.contains("react") {
                    frameworks.insert("react".into());
                }
                if text.contains("vue") {
                    frameworks.insert("vue".into());
                }
            }
        }
        Some("pyproject.toml") | Some("requirements.txt") | Some("setup.py") => {
            package_managers.insert("pip".into());
        }
        Some("go.mod") => {
            package_managers.insert("go modules".into());
        }
        Some("Gemfile") => {
            package_managers.insert("bundler".into());
        }
        _ => {}
    }
}

fn preview_text(text: &str) -> String {
    let lines: Vec<&str> = text.lines().take(PREVIEW_LINES).collect();
    lines.join("\n")
}

fn estimate_tokens(bytes: usize) -> usize {
    bytes / 4
}

fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .map(str::trim)
        .filter(|token| !token.is_empty() && token.len() > 1)
        .map(String::from)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

fn score_entry(entry: &FileEntry, terms: &[String]) -> (f64, Vec<String>) {
    let mut score = 0.0;
    let mut matched = Vec::new();
    let path_lower = entry.path.to_lowercase();
    let preview_lower = entry.preview.to_lowercase();
    let language = entry.language.as_deref().unwrap_or("").to_lowercase();

    for term in terms {
        let term_score = if path_lower.contains(term) {
            if path_lower == *term {
                10.0
            } else if entry.path.to_lowercase().ends_with(term) {
                5.0
            } else {
                2.0
            }
        } else if preview_lower.contains(term) {
            1.0
        } else if language.contains(term) {
            0.5
        } else {
            0.0
        };

        if term_score > 0.0 {
            score += term_score;
            matched.push(term.clone());
        }
    }

    (score, matched)
}

pub fn validate_relative(path: &Path, workspace: &Path) -> Result<PathBuf, ContextError> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(ContextError::PathEscape(path.to_path_buf()));
    }
    Ok(workspace.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn index_classifies_rust_files_and_detects_cargo() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname='fixture'\n",
        )
        .unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .unwrap();
        fs::create_dir(temp.path().join("target")).unwrap();
        fs::write(temp.path().join("target/artifact"), "binary").unwrap();

        let engine = ContextEngine::index(temp.path().to_path_buf()).unwrap();
        assert!(engine.profile.languages.contains(&"rust".into()));
        assert!(engine.profile.package_managers.contains(&"cargo".into()));
        assert!(engine.files.iter().any(|entry| entry.path == "src/lib.rs"));
        assert!(
            !engine
                .files
                .iter()
                .any(|entry| entry.path.starts_with("target/"))
        );
    }

    #[test]
    fn query_matches_path_and_preview() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("Cargo.toml"), "[package]\n").unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/agent.rs"), "pub struct Agent;\n").unwrap();
        fs::write(
            temp.path().join("src/tools.rs"),
            "pub struct ToolRegistry;\n",
        )
        .unwrap();

        let engine = ContextEngine::index(temp.path().to_path_buf()).unwrap();
        let results = engine.query("agent", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "src/agent.rs");
        assert!(results[0].explanation.contains("agent"));

        let results = engine.query("tool registry", 5);
        assert!(results.iter().any(|result| result.path == "src/tools.rs"));
    }

    #[test]
    fn ignores_git_and_omni_directories() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".git/config"), "\n").unwrap();
        fs::create_dir(temp.path().join(".omni")).unwrap();
        fs::write(temp.path().join(".omni/state"), "\n").unwrap();
        fs::write(temp.path().join("visible.txt"), "hello").unwrap();

        let engine = ContextEngine::index(temp.path().to_path_buf()).unwrap();
        assert!(engine.files.iter().any(|entry| entry.path == "visible.txt"));
        assert!(
            !engine
                .files
                .iter()
                .any(|entry| entry.path.contains(".git/"))
        );
        assert!(
            !engine
                .files
                .iter()
                .any(|entry| entry.path.contains(".omni/"))
        );
    }

    #[test]
    fn respects_custom_omniignore() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join(".omniignore"), "secrets/\n").unwrap();
        fs::create_dir(temp.path().join("secrets")).unwrap();
        fs::write(temp.path().join("secrets/key.txt"), "secret").unwrap();
        fs::write(temp.path().join("public.txt"), "ok").unwrap();

        let engine = ContextEngine::index(temp.path().to_path_buf()).unwrap();
        assert!(engine.files.iter().any(|entry| entry.path == "public.txt"));
        assert!(
            !engine
                .files
                .iter()
                .any(|entry| entry.path.contains("secrets/"))
        );
    }
}
