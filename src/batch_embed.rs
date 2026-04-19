use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

use crate::embeddings::{AnyEmbeddingProvider, EmbeddingProvider};

#[derive(sqlx::FromRow)]
struct StaleRow {
    id: Uuid,
    text: String,
}

/// Re-embed rules and attempt reasoning that have NULL embeddings.
/// Runs once on startup as a background task.
pub async fn backfill_embeddings(pool: PgPool, provider: AnyEmbeddingProvider) {
    // Short delay to let the server finish initializing
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    if let Err(e) = backfill_rules(&pool, &provider).await {
        tracing::warn!(error = %e, "Failed to backfill rule embeddings");
    }
    if let Err(e) = backfill_attempts(&pool, &provider).await {
        tracing::warn!(error = %e, "Failed to backfill attempt embeddings");
    }
}

async fn backfill_rules(
    pool: &PgPool,
    provider: &AnyEmbeddingProvider,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let stale: Vec<StaleRow> = sqlx::query_as(
        "SELECT id, content AS text FROM ai_memory.semantic_rules \
         WHERE embedding IS NULL LIMIT 100",
    )
    .fetch_all(pool)
    .await?;

    if stale.is_empty() {
        return Ok(());
    }
    tracing::debug!(count = stale.len(), "Backfilling rule embeddings");

    // Process in batches of 10
    for chunk in stale.chunks(10) {
        let texts: Vec<&str> = chunk.iter().map(|r| r.text.as_str()).collect();
        let embeddings = provider.embed_batch(&texts).await?;
        for (row, emb) in chunk.iter().zip(embeddings.iter()) {
            let vec = Vector::from(emb.clone());
            sqlx::query("UPDATE ai_memory.semantic_rules SET embedding = $2 WHERE id = $1")
                .bind(row.id)
                .bind(&vec)
                .execute(pool)
                .await?;
        }
    }
    tracing::debug!(count = stale.len(), "Rule embeddings backfilled");
    Ok(())
}

async fn backfill_attempts(
    pool: &PgPool,
    provider: &AnyEmbeddingProvider,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let stale: Vec<StaleRow> = sqlx::query_as(
        "SELECT id, reasoning AS text FROM ai_memory.attempts \
         WHERE reasoning != '' AND reasoning_embedding IS NULL LIMIT 100",
    )
    .fetch_all(pool)
    .await?;

    if stale.is_empty() {
        return Ok(());
    }
    tracing::debug!(count = stale.len(), "Backfilling attempt embeddings");

    for chunk in stale.chunks(10) {
        let texts: Vec<&str> = chunk.iter().map(|r| r.text.as_str()).collect();
        let embeddings = provider.embed_batch(&texts).await?;
        for (row, emb) in chunk.iter().zip(embeddings.iter()) {
            let vec = Vector::from(emb.clone());
            sqlx::query("UPDATE ai_memory.attempts SET reasoning_embedding = $2 WHERE id = $1")
                .bind(row.id)
                .bind(&vec)
                .execute(pool)
                .await?;
        }
    }
    tracing::debug!(count = stale.len(), "Attempt embeddings backfilled");
    Ok(())
}
