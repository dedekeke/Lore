use std::collections::HashMap;
use std::path::Path;

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::db;
use crate::embeddings::{AnyEmbeddingProvider, EmbeddingProvider};

/// Detect language from file extension
pub fn detect_language(path: &str) -> Option<String> {
    let ext = Path::new(path).extension()?.to_str()?;
    let lang = match ext {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "jsx" | "mjs" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "sql" => "sql",
        "sh" | "bash" | "zsh" => "shell",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "json" => "json",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" => "css",
        _ => return None,
    };
    Some(lang.to_string())
}

/// Language-aware boundary patterns for chunking
fn boundary_patterns(language: &str) -> Vec<&'static str> {
    match language {
        "rust" => vec![
            "pub fn ",
            "fn ",
            "pub struct ",
            "struct ",
            "pub enum ",
            "enum ",
            "impl ",
            "pub trait ",
            "trait ",
            "pub mod ",
            "mod ",
            "pub const ",
            "pub type ",
        ],
        "python" => vec!["def ", "async def ", "class "],
        "javascript" | "typescript" => vec![
            "function ",
            "export function ",
            "export default function ",
            "class ",
            "export class ",
            "const ",
            "export const ",
        ],
        "go" => vec!["func ", "type "],
        "java" | "kotlin" => vec![
            "public class ",
            "class ",
            "public interface ",
            "interface ",
            "public ",
            "private ",
            "protected ",
        ],
        "ruby" => vec!["def ", "class ", "module "],
        "php" => vec!["function ", "class ", "interface "],
        "c" | "cpp" => vec!["void ", "int ", "char ", "struct ", "class ", "namespace "],
        "swift" => vec!["func ", "class ", "struct ", "enum ", "protocol "],
        _ => vec![],
    }
}

/// Split file content into language-aware chunks
pub fn chunk_file(content: &str, language: Option<&str>, max_lines: usize) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return vec![];
    }

    let patterns = language.map(boundary_patterns).unwrap_or_default();

    if patterns.is_empty() {
        return chunk_by_size(&lines, max_lines);
    }

    let mut boundaries = vec![0usize]; // always start at line 0
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if i > 0 && patterns.iter().any(|p| trimmed.starts_with(p)) {
            boundaries.push(i);
        }
    }

    let mut chunks = Vec::new();
    for window in boundaries.windows(2) {
        let start = window[0];
        let end = window[1];
        // If this chunk is too large, split it further
        if end - start > max_lines {
            let sub_lines = &lines[start..end];
            for (sub_start, sub_end) in chunk_by_size(sub_lines, max_lines) {
                chunks.push((start + sub_start, start + sub_end));
            }
        } else {
            chunks.push((start, end));
        }
    }

    // Last boundary to end of file
    let last = *boundaries.last().unwrap();
    if last < lines.len() {
        let remaining = &lines[last..];
        if remaining.len() > max_lines {
            for (sub_start, sub_end) in chunk_by_size(remaining, max_lines) {
                chunks.push((last + sub_start, last + sub_end));
            }
        } else {
            chunks.push((last, lines.len()));
        }
    }

    // Filter out empty chunks
    chunks.retain(|(s, e)| e > s);
    chunks
}

const MAX_CHUNK_LINES: usize = 80;

/// Fallback: split by fixed size
fn chunk_by_size(lines: &[&str], max_lines: usize) -> Vec<(usize, usize)> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let end = (start + max_lines).min(lines.len());
        chunks.push((start, end));
        start = end;
    }
    chunks
}

pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

/// Scan project directory, chunk files, embed, and store in DB
pub async fn index_codebase(
    pool: &PgPool,
    embeddings: &AnyEmbeddingProvider,
    project_id: Uuid,
    root_path: &str,
    patterns: Option<&[String]>,
) -> Result<IndexResult, IndexError> {
    let root = Path::new(root_path);
    if !root.is_dir() {
        return Err(IndexError::InvalidPath(root_path.to_string()));
    }

    let mut builder = ignore::WalkBuilder::new(root);
    builder.hidden(true).git_ignore(true).git_global(true);
    if let Some(pats) = patterns {
        for pat in pats {
            let _ = builder.add_ignore(pat);
        }
    }

    let existing_hashes: HashMap<String, String> = db::codebase::get_file_hashes(pool, project_id)
        .await
        .map_err(IndexError::Db)?
        .into_iter()
        .collect();

    // Collect changed files and their chunks (no DB writes yet)
    let mut all_chunks: Vec<db::codebase::NewCodeChunk> = Vec::new();
    let mut current_files: Vec<String> = Vec::new();
    let mut changed_files: Vec<String> = Vec::new();
    let mut files_scanned = 0u64;
    let mut files_changed = 0u64;
    let mut files_skipped = 0u64;

    for entry in builder.build().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if is_binary_ext(ext) {
            continue;
        }

        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        if should_skip_path(&rel_path) {
            continue;
        }

        files_scanned += 1;
        current_files.push(rel_path.clone());

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(path = %rel_path, error = %e, "Failed to read file, skipping");
                continue;
            }
        };

        let hash = sha256_hex(&content);

        if existing_hashes.get(&rel_path).map(|h| h.as_str()) == Some(&hash) {
            files_skipped += 1;
            continue;
        }
        files_changed += 1;
        changed_files.push(rel_path.clone());

        let language = detect_language(&rel_path);
        let ranges = chunk_file(&content, language.as_deref(), MAX_CHUNK_LINES);
        let lines: Vec<&str> = content.lines().collect();

        for (start, end) in ranges {
            let chunk_content: String = lines[start..end].join("\n");
            if chunk_content.trim().is_empty() {
                continue;
            }

            all_chunks.push(db::codebase::NewCodeChunk {
                file_path: rel_path.clone(),
                start_line: (start + 1) as i32,
                end_line: end as i32,
                language: language.clone(),
                content: chunk_content,
                embedding: None,
                file_hash: hash.clone(),
            });
        }
    }

    let stale_deleted = db::codebase::delete_stale_files(pool, project_id, &current_files)
        .await
        .map_err(IndexError::Db)?;

    // Batch embed in groups of 64 (provider-level batching)
    let mut embed_errors = 0u64;
    for batch in all_chunks.chunks_mut(64) {
        let texts: Vec<&str> = batch.iter().map(|c| c.content.as_str()).collect();
        match embeddings.embed_batch(&texts).await {
            Ok(embeddings_vec) => {
                for (chunk, emb) in batch.iter_mut().zip(embeddings_vec) {
                    chunk.embedding = Some(emb);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Batch embed failed for {} chunks", batch.len());
                embed_errors += batch.len() as u64;
            }
        }
    }

    // Upsert first (COALESCE preserves existing embeddings on embed failure),
    // then delete stale start_lines — this ordering prevents data loss on crash
    let chunks_inserted = db::codebase::insert_chunks(pool, project_id, &all_chunks)
        .await
        .map_err(IndexError::Db)?;

    // Delete stale chunks whose start_lines no longer exist after re-chunking
    for file_path in &changed_files {
        let valid_starts: Vec<i32> = all_chunks
            .iter()
            .filter(|c| c.file_path == *file_path)
            .map(|c| c.start_line)
            .collect();
        let _ = db::codebase::delete_stale_start_lines(pool, project_id, file_path, &valid_starts)
            .await
            .map_err(IndexError::Db)?;
    }

    Ok(IndexResult {
        files_scanned,
        files_changed,
        files_skipped,
        chunks_indexed: chunks_inserted,
        stale_files_removed: stale_deleted,
        embed_errors,
    })
}

fn is_binary_ext(ext: &str) -> bool {
    matches!(
        ext,
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "ico"
            | "svg"
            | "webp"
            | "mp3"
            | "mp4"
            | "wav"
            | "avi"
            | "mov"
            | "zip"
            | "gz"
            | "tar"
            | "bz2"
            | "xz"
            | "7z"
            | "rar"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "o"
            | "a"
            | "wasm"
            | "pdf"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ttf"
            | "otf"
            | "woff"
            | "woff2"
            | "eot"
            | "pyc"
            | "pyo"
            | "class"
            | "jar"
            | "db"
            | "sqlite"
            | "sqlite3"
            | "bin"
            | "dat"
            | "onnx"
            | "pt"
            | "pb"
    )
}

fn should_skip_path(path: &str) -> bool {
    let skip_prefixes = [
        "target/",
        "node_modules/",
        ".git/",
        "__pycache__/",
        ".venv/",
        "venv/",
        "dist/",
        "build/",
        ".next/",
        ".nuxt/",
        "vendor/",
        ".cargo/",
        "pkg/",
    ];
    let skip_files = [
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        ".DS_Store",
        "Thumbs.db",
    ];
    skip_prefixes.iter().any(|p| path.starts_with(p))
        || skip_files.iter().any(|f| path.ends_with(f))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexResult {
    pub files_scanned: u64,
    pub files_changed: u64,
    pub files_skipped: u64,
    pub chunks_indexed: u64,
    pub stale_files_removed: u64,
    pub embed_errors: u64,
}

#[derive(Debug)]
pub enum IndexError {
    InvalidPath(String),
    Db(sqlx::Error),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(p) => write!(f, "Invalid path: {p}"),
            Self::Db(e) => write!(f, "Database error: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language("src/main.rs"), Some("rust".to_string()));
        assert_eq!(detect_language("app.py"), Some("python".to_string()));
        assert_eq!(detect_language("index.ts"), Some("typescript".to_string()));
        assert_eq!(detect_language("README.md"), Some("markdown".to_string()));
        assert_eq!(detect_language("no_ext"), None);
    }

    #[test]
    fn test_chunk_rust_file() {
        let content = "\
use std::io;

pub fn hello() {
    println!(\"hello\");
}

pub fn world() {
    println!(\"world\");
}

impl Foo {
    fn bar() {}
}";
        let chunks = chunk_file(content, Some("rust"), 80);
        assert!(chunks.len() >= 3); // imports+hello, world, impl Foo
    }

    #[test]
    fn test_chunk_fallback() {
        let content = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_file(&content, None, 80);
        assert_eq!(chunks.len(), 3); // 80 + 80 + 40
    }

    #[test]
    fn test_sha256() {
        let hash = sha256_hex("hello world");
        assert_eq!(hash.len(), 64);
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_should_skip_path() {
        assert!(should_skip_path("target/debug/main"));
        assert!(should_skip_path("node_modules/foo/bar.js"));
        assert!(!should_skip_path("src/main.rs"));
    }
}
