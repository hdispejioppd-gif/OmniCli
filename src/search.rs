//! Full-text code search backed by tantivy (BM25).

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{STORED, Schema, TEXT, Value};
use tantivy::{Index, IndexWriter, TantivyDocument, doc};

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("tantivy: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("query parse: {0}")]
    Query(#[from] tantivy::query::QueryParserError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, serde::Serialize)]
pub struct SearchHit {
    pub path: PathBuf,
    pub score: f32,
    /// Up to 3 matching lines with line numbers.
    pub snippet: String,
}

pub struct CodeIndex {
    index: Index,
    path_field: tantivy::schema::Field,
    body_field: tantivy::schema::Field,
    content_field: tantivy::schema::Field,
}

impl CodeIndex {
    pub fn open(data_dir: &Path) -> Result<Self, SearchError> {
        let mut schema_builder = Schema::builder();
        let path_field = schema_builder.add_text_field("path", TEXT | STORED);
        let body_field = schema_builder.add_text_field("body", TEXT);
        let content_field = schema_builder.add_text_field("content_stored", STORED);
        let schema = schema_builder.build();
        let index_dir = data_dir.join("index");
        std::fs::create_dir_all(&index_dir)?;
        let index = if index_dir.join("meta.json").exists() {
            Index::open_in_dir(&index_dir)?
        } else {
            Index::create_in_dir(&index_dir, schema.clone())?
        };
        Ok(Self {
            index,
            path_field,
            body_field,
            content_field,
        })
    }

    pub fn reindex(&self, workspace: &Path, max_file_bytes: u64) -> Result<usize, SearchError> {
        let mut writer: IndexWriter = self.index.writer(50_000_000)?;
        writer.delete_all_documents()?;

        let walker = WalkBuilder::new(workspace)
            .hidden(true)
            .git_ignore(true)
            .build();
        let mut count = 0usize;

        for entry in walker.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(meta) = path.metadata()
                && meta.len() > max_file_bytes
            {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(path) else {
                continue;
            };
            let rel = path.strip_prefix(workspace).unwrap_or(path);
            let _ = writer.add_document(doc!(
                self.path_field => rel.to_string_lossy().as_ref(),
                self.body_field => source.as_str(),
                self.content_field => source.as_str(),
            ))?;
            count += 1;
        }
        writer.commit()?;
        Ok(count)
    }

    pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();

        let query_parser = QueryParser::for_index(&self.index, vec![self.body_field]);
        let query = query_parser.parse_query(query_str)?;

        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;

        let terms: Vec<&str> = query_str.split_whitespace().collect();
        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc::<TantivyDocument>(doc_address)?;
            let path = doc
                .get_first(self.path_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = doc
                .get_first(self.content_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = snippet_from_content(&content, &terms);

            hits.push(SearchHit {
                path: PathBuf::from(path),
                score,
                snippet,
            });
        }
        Ok(hits)
    }
}

fn snippet_from_content(content: &str, terms: &[&str]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut matches = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        if terms.is_empty()
            || terms
                .iter()
                .any(|t| line.to_lowercase().contains(&t.to_lowercase()))
        {
            matches.push(format!("{}: {}", i + 1, line));
            if matches.len() >= 3 {
                break;
            }
        }
    }
    matches.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reindex_and_search() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("tax.rs"), "fn calculate_tax() -> u32 { 42 }").unwrap();
        std::fs::write(ws.path().join("zz.rs"), "struct Zzz;").unwrap();

        let index_dir = tempfile::tempdir().unwrap();
        let idx = CodeIndex::open(index_dir.path()).unwrap();
        let n = idx.reindex(ws.path(), 1_000_000).unwrap();
        assert_eq!(n, 2);

        let hits = idx.search("calculate tax", 5).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].path, PathBuf::from("tax.rs"));
    }

    #[test]
    fn search_returns_snippet_with_line_numbers() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join("lib.rs"),
            "// header\nfn calc_tax() -> u32 {\n    42\n}\n",
        )
        .unwrap();

        let index_dir = tempfile::tempdir().unwrap();
        let idx = CodeIndex::open(index_dir.path()).unwrap();
        idx.reindex(ws.path(), 1_000_000).unwrap();

        let hits = idx.search("calc_tax", 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("2: "));
        assert!(hits[0].snippet.contains("calc_tax"));
    }

    #[test]
    fn reindex_is_idempotent() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("a.rs"), "fn a() {}").unwrap();
        std::fs::write(ws.path().join("b.rs"), "fn b() {}").unwrap();

        let index_dir = tempfile::tempdir().unwrap();
        let idx = CodeIndex::open(index_dir.path()).unwrap();
        let n1 = idx.reindex(ws.path(), 1_000_000).unwrap();
        let n2 = idx.reindex(ws.path(), 1_000_000).unwrap();
        assert_eq!(n1, 2);
        assert_eq!(n2, 2);
    }

    #[test]
    fn binary_files_skipped() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("a.rs"), "fn a() {}").unwrap();
        std::fs::write(ws.path().join("img.png"), b"\x89PNG\r\n\x1a\n").unwrap();

        let index_dir = tempfile::tempdir().unwrap();
        let idx = CodeIndex::open(index_dir.path()).unwrap();
        let n = idx.reindex(ws.path(), 1_000_000).unwrap();
        assert_eq!(n, 1);
    }
}
