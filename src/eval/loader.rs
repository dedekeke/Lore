use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use super::types::EvalCase;

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("http: {0}")]
    Http(String),
    #[error("sha256 mismatch: expected {expected}, got {got}")]
    ShaMismatch { expected: String, got: String },
}

/// Declarative description of a real dataset (LoCoMo / LongMemEval / BEAM).
/// P1-T1 ships the type; actual loaders land in a follow-up PR.
#[derive(Debug, Clone)]
pub struct DatasetSource {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub license: &'static str,
}

pub trait DatasetLoader {
    fn name(&self) -> &str;
    fn load(&self) -> Result<Vec<EvalCase>, EvalError>;
}

/// Download-if-missing helper with sha256 verification.
/// Used by real dataset loaders (LoCoMo/LongMemEval/BEAM) in a follow-up task.
pub fn fetch_cached(src: &DatasetSource, cache_dir: &Path) -> Result<PathBuf, EvalError> {
    fs::create_dir_all(cache_dir)?;
    let dest = cache_dir.join(format!("{}.bin", src.name));
    if dest.exists() && verify_sha256(&dest, src.sha256)? {
        return Ok(dest);
    }
    let resp = reqwest::blocking::get(src.url)
        .map_err(|e| EvalError::Http(format!("GET {}: {e}", src.url)))?;
    if !resp.status().is_success() {
        return Err(EvalError::Http(format!(
            "GET {}: HTTP {}",
            src.url,
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .map_err(|e| EvalError::Http(format!("read body: {e}")))?;
    let tmp = dest.with_extension("tmp");
    fs::write(&tmp, &bytes)?;
    if !verify_sha256(&tmp, src.sha256)? {
        let got = hex_sha256(&tmp)?;
        let _ = fs::remove_file(&tmp);
        return Err(EvalError::ShaMismatch {
            expected: src.sha256.to_string(),
            got,
        });
    }
    fs::rename(&tmp, &dest)?;
    Ok(dest)
}

fn verify_sha256(path: &Path, expected: &str) -> Result<bool, EvalError> {
    Ok(hex_sha256(path)?.eq_ignore_ascii_case(expected))
}

fn hex_sha256(path: &Path) -> Result<String, EvalError> {
    let bytes = fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(hex::encode_digest(&h.finalize()))
}

mod hex {
    pub fn encode_digest(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}
