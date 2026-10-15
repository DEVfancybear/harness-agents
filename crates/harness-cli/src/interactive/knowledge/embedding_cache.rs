//! Bounded persistent vectors. The wiki remains the source of truth.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use super::{MAX_VECTOR_BYTES, cache_path, decode_vector, encode_vector};

const QUERY_TTL_SECONDS: i64 = 3600;
const MAX_QUERIES: i64 = 128;

pub(super) struct Cache {
    pool: SqlitePool,
}

impl Cache {
    pub(super) async fn open(root: &Path) -> Result<Self, String> {
        let path = cache_path(root)?;
        std::fs::create_dir_all(root).map_err(|_| "cannot create embedding cache directory")?;
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Delete)
            .busy_timeout(Duration::from_secs(5))
            .pragma("max_page_count", "10240");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|_| "cannot open embedding cache")?;
        for statement in [
            "CREATE TABLE IF NOT EXISTS embedding_meta (namespace TEXT PRIMARY KEY, dimensions INTEGER NOT NULL)",
            "CREATE TABLE IF NOT EXISTS embedding_vectors (namespace TEXT NOT NULL, chunk_key TEXT NOT NULL, vector BLOB NOT NULL, PRIMARY KEY(namespace, chunk_key))",
            "CREATE TABLE IF NOT EXISTS embedding_queries (namespace TEXT NOT NULL, query_hash TEXT NOT NULL, dimensions INTEGER NOT NULL, created_at INTEGER NOT NULL, vector BLOB NOT NULL, PRIMARY KEY(namespace, query_hash))",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .map_err(db_error)?;
        }
        Ok(Self { pool })
    }

    pub(super) async fn reconcile(
        &self,
        namespace: &str,
        desired: &BTreeSet<String>,
    ) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        for statement in [
            "DELETE FROM embedding_vectors WHERE namespace != ?",
            "DELETE FROM embedding_queries WHERE namespace != ?",
            "DELETE FROM embedding_meta WHERE namespace != ?",
        ] {
            sqlx::query(statement)
                .bind(namespace)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        let keys = sqlx::query("SELECT chunk_key FROM embedding_vectors WHERE namespace = ?")
            .bind(namespace)
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?;
        for row in keys {
            let key: String = row.try_get("chunk_key").map_err(db_error)?;
            if !desired.contains(&key) {
                sqlx::query("DELETE FROM embedding_vectors WHERE namespace = ? AND chunk_key = ?")
                    .bind(namespace)
                    .bind(key)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_error)?;
            }
        }
        sqlx::query("DELETE FROM embedding_queries WHERE created_at < ?")
            .bind(now() - QUERY_TTL_SECONDS)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    pub(super) async fn load(
        &self,
        namespace: &str,
    ) -> Result<(usize, HashMap<String, Vec<f32>>), String> {
        let dimensions: Option<i64> =
            sqlx::query_scalar("SELECT dimensions FROM embedding_meta WHERE namespace = ?")
                .bind(namespace)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_error)?;
        let dimensions = usize::try_from(dimensions.unwrap_or_default())
            .map_err(|_| "invalid cached embedding dimensions")?;
        let rows =
            sqlx::query("SELECT chunk_key, vector FROM embedding_vectors WHERE namespace = ?")
                .bind(namespace)
                .fetch_all(&self.pool)
                .await
                .map_err(db_error)?;
        let mut vectors = HashMap::new();
        for row in rows {
            let key = row.try_get("chunk_key").map_err(db_error)?;
            let bytes: Vec<u8> = row.try_get("vector").map_err(db_error)?;
            vectors.insert(key, decode_vector(&bytes, dimensions)?);
        }
        Ok((dimensions, vectors))
    }

    pub(super) async fn clear(&self, namespace: &str) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        for statement in [
            "DELETE FROM embedding_vectors WHERE namespace = ?",
            "DELETE FROM embedding_queries WHERE namespace = ?",
            "DELETE FROM embedding_meta WHERE namespace = ?",
        ] {
            sqlx::query(statement)
                .bind(namespace)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    pub(super) async fn put_vectors(
        &self,
        namespace: &str,
        dimensions: usize,
        entries: &[(String, Vec<f32>)],
    ) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let existing: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(length(vector)), 0) FROM embedding_vectors")
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        let additional: usize = entries.iter().map(|(_, vector)| vector.len() * 4).sum();
        if usize::try_from(existing).unwrap_or(usize::MAX) + additional > MAX_VECTOR_BYTES {
            return Err("embedding vector cache exceeds its 32 MiB limit".to_owned());
        }
        sqlx::query("INSERT OR REPLACE INTO embedding_meta(namespace, dimensions) VALUES(?, ?)")
            .bind(namespace)
            .bind(i64::try_from(dimensions).map_err(|_| "embedding dimensions overflow")?)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        for (key, vector) in entries {
            sqlx::query("INSERT OR REPLACE INTO embedding_vectors(namespace, chunk_key, vector) VALUES(?, ?, ?)")
                .bind(namespace)
                .bind(key)
                .bind(encode_vector(vector))
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }

    pub(super) async fn query(
        &self,
        namespace: &str,
        hash: &str,
        dimensions: usize,
    ) -> Result<Option<Vec<f32>>, String> {
        let row = sqlx::query("SELECT vector FROM embedding_queries WHERE namespace = ? AND query_hash = ? AND dimensions = ? AND created_at >= ?")
            .bind(namespace)
            .bind(hash)
            .bind(i64::try_from(dimensions).map_err(|_| "embedding dimensions overflow")?)
            .bind(now() - QUERY_TTL_SECONDS)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?;
        row.map(|row| {
            let bytes: Vec<u8> = row.try_get("vector").map_err(db_error)?;
            decode_vector(&bytes, dimensions)
        })
        .transpose()
    }

    pub(super) async fn put_query(
        &self,
        namespace: &str,
        hash: &str,
        vector: &[f32],
    ) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        sqlx::query("DELETE FROM embedding_queries WHERE namespace = ? AND query_hash IN (SELECT query_hash FROM embedding_queries WHERE namespace = ? ORDER BY created_at DESC, query_hash LIMIT -1 OFFSET ?)")
            .bind(namespace)
            .bind(namespace)
            .bind(MAX_QUERIES - 1)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let document_bytes: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(length(vector)), 0) FROM embedding_vectors")
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        let query_bytes: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(length(vector)), 0) FROM embedding_queries")
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        let required = vector.len() * 4;
        if usize::try_from(document_bytes)
            .unwrap_or(usize::MAX)
            .saturating_add(required)
            > MAX_VECTOR_BYTES
        {
            return Ok(());
        }
        if usize::try_from(document_bytes.saturating_add(query_bytes))
            .unwrap_or(usize::MAX)
            .saturating_add(required)
            > MAX_VECTOR_BYTES
        {
            sqlx::query("DELETE FROM embedding_queries")
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("INSERT OR REPLACE INTO embedding_queries(namespace, query_hash, dimensions, created_at, vector) VALUES(?, ?, ?, ?, ?)")
            .bind(namespace)
            .bind(hash)
            .bind(i64::try_from(vector.len()).map_err(|_| "embedding dimensions overflow")?)
            .bind(now())
            .bind(encode_vector(vector))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
}

fn now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}

fn db_error(_: sqlx::Error) -> String {
    "embedding cache operation failed".to_owned()
}
