use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ndarray::{Array2, ArrayView3, Axis};
use ort::session::Session;
use ort::value::TensorRef;

use super::{EmbeddingError, EmbeddingProvider};

const HF_BASE: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main";

pub struct LocalEmbeddingProvider {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<tokenizers::Tokenizer>,
    dimensions: usize,
}

impl LocalEmbeddingProvider {
    pub fn new(model_name: &str, dimensions: usize) -> Result<Self, EmbeddingError> {
        let cache_dir = model_cache_dir(model_name);
        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| EmbeddingError::Model(format!("Failed to create cache dir: {e}")))?;

        let model_path = cache_dir.join("model.onnx");
        let tokenizer_path = cache_dir.join("tokenizer.json");

        if !model_path.exists() || !tokenizer_path.exists() {
            tracing::info!(model = model_name, "Downloading ONNX model and tokenizer");
            download_file(&format!("{HF_BASE}/onnx/model.onnx"), &model_path)?;
            download_file(&format!("{HF_BASE}/tokenizer.json"), &tokenizer_path)?;
            tracing::info!("Download complete");
        }

        let session = Session::builder()
            .map_err(|e| EmbeddingError::Model(e.to_string()))?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|e| EmbeddingError::Model(e.to_string()))?
            .with_intra_threads(4)
            .map_err(|e| EmbeddingError::Model(e.to_string()))?
            .commit_from_file(&model_path)
            .map_err(|e| EmbeddingError::Model(e.to_string()))?;

        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| EmbeddingError::Model(format!("Failed to load tokenizer: {e}")))?;

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
            dimensions,
        })
    }
}

impl EmbeddingProvider for LocalEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let session = self.session.clone();
        let tokenizer = self.tokenizer.clone();
        let dims = self.dimensions;
        let text = text.to_string();

        tokio::task::spawn_blocking(move || {
            let mut results = run_inference(&session, &tokenizer, &[&text], dims)?;
            results
                .pop()
                .ok_or_else(|| EmbeddingError::Model("No embedding returned".to_string()))
        })
        .await
        .map_err(|e| EmbeddingError::Model(format!("Blocking task failed: {e}")))?
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let session = self.session.clone();
        let tokenizer = self.tokenizer.clone();
        let dims = self.dimensions;
        let owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();

        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
            run_inference(&session, &tokenizer, &refs, dims)
        })
        .await
        .map_err(|e| EmbeddingError::Model(format!("Blocking task failed: {e}")))?
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

fn run_inference(
    session: &Mutex<Session>,
    tokenizer: &tokenizers::Tokenizer,
    texts: &[&str],
    dimensions: usize,
) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    let encodings = tokenizer
        .encode_batch(texts.to_vec(), true)
        .map_err(|e| EmbeddingError::Model(format!("Tokenization failed: {e}")))?;

    let batch = encodings.len();
    let seq_len = encodings.iter().map(|e| e.len()).max().unwrap_or(0);

    let mut ids = vec![0i64; batch * seq_len];
    let mut mask = vec![0i64; batch * seq_len];
    let mut type_ids = vec![0i64; batch * seq_len];

    for (i, enc) in encodings.iter().enumerate() {
        let offset = i * seq_len;
        for (j, (&id, &m)) in enc
            .get_ids()
            .iter()
            .zip(enc.get_attention_mask().iter())
            .enumerate()
        {
            ids[offset + j] = id as i64;
            mask[offset + j] = m as i64;
        }
        for (j, &t) in enc.get_type_ids().iter().enumerate() {
            type_ids[offset + j] = t as i64;
        }
    }

    let t_ids = TensorRef::from_array_view(([batch, seq_len], &*ids))
        .map_err(|e| EmbeddingError::Model(e.to_string()))?;
    let t_mask = TensorRef::from_array_view(([batch, seq_len], &*mask))
        .map_err(|e| EmbeddingError::Model(e.to_string()))?;
    let t_type = TensorRef::from_array_view(([batch, seq_len], &*type_ids))
        .map_err(|e| EmbeddingError::Model(e.to_string()))?;

    let mut session = session
        .lock()
        .map_err(|e| EmbeddingError::Model(format!("Session lock poisoned: {e}")))?;

    let outputs = session
        .run(ort::inputs![
            "input_ids" => t_ids,
            "attention_mask" => t_mask,
            "token_type_ids" => t_type
        ])
        .map_err(|e| EmbeddingError::Model(e.to_string()))?;

    let token_embeddings = outputs[0]
        .try_extract_array::<f32>()
        .map_err(|e| EmbeddingError::Model(e.to_string()))?;
    let token_view = token_embeddings
        .view()
        .into_dimensionality::<ndarray::Ix3>()
        .map_err(|e| EmbeddingError::Model(e.to_string()))?;

    let mask_arr = Array2::from_shape_vec((batch, seq_len), mask)
        .map_err(|e| EmbeddingError::Model(e.to_string()))?;

    let pooled = mean_pool(token_view, &mask_arr);
    let normalized = l2_normalize(pooled);

    let mut results = Vec::with_capacity(batch);
    for row in normalized.rows() {
        let vec: Vec<f32> = row.to_vec();
        if vec.len() != dimensions {
            return Err(EmbeddingError::DimensionMismatch {
                expected: dimensions,
                actual: vec.len(),
            });
        }
        results.push(vec);
    }

    Ok(results)
}

fn mean_pool(token_embeddings: ArrayView3<f32>, attention_mask: &Array2<i64>) -> Array2<f32> {
    let mask_expanded = attention_mask
        .clone()
        .insert_axis(Axis(2))
        .broadcast(token_embeddings.dim())
        .unwrap()
        .mapv(|x| x as f32)
        .to_owned();
    let masked = &mask_expanded * &token_embeddings;
    let sum = masked.sum_axis(Axis(1));
    let denom = mask_expanded
        .sum_axis(Axis(1))
        .mapv(|x| if x == 0.0 { 1.0 } else { x });
    sum / denom
}

fn l2_normalize(embeddings: Array2<f32>) -> Array2<f32> {
    let norms = embeddings.mapv(|x| x * x).sum_axis(Axis(1)).mapv(f32::sqrt);
    let norms = norms.mapv(|x| if x == 0.0 { 1.0 } else { x });
    let norms = norms.insert_axis(Axis(1));
    let norms = norms.broadcast(embeddings.dim()).unwrap().to_owned();
    embeddings / norms
}

fn model_cache_dir(model_name: &str) -> PathBuf {
    dirs_next::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("lore")
        .join("models")
        .join(model_name)
}

fn download_file(url: &str, dest: &PathBuf) -> Result<(), EmbeddingError> {
    let response = reqwest::blocking::get(url)
        .map_err(|e| EmbeddingError::Api(format!("Download failed for {url}: {e}")))?;

    if !response.status().is_success() {
        return Err(EmbeddingError::Api(format!(
            "Download failed for {url}: HTTP {}",
            response.status()
        )));
    }

    let bytes = response
        .bytes()
        .map_err(|e| EmbeddingError::Api(format!("Failed to read response body: {e}")))?;

    // Atomic write by creating a temporary file first
    let tmp_dest = dest.with_extension("tmp");
    std::fs::write(&tmp_dest, &bytes)
        .map_err(|e| EmbeddingError::Model(format!("Failed to write {}: {e}", tmp_dest.display())))?;
        
    std::fs::rename(&tmp_dest, dest)
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp_dest);
            EmbeddingError::Model(format!("Failed to rename {} to {}: {e}", tmp_dest.display(), dest.display()))
        })?;

    Ok(())
}
