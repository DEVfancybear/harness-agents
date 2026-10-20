//! Explicitly enabled OpenAI-compatible embeddings; the default is local keyword retrieval.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

#[path = "embedding_cache.rs"]
mod cache;

const CONFIG_FILE: &str = ".ha-embedding.json";
const CACHE_FILE: &str = ".ha-embedding.sqlite";
const MAX_CONFIG: usize = 8192;
const MAX_DOCUMENTS: usize = 2048;
const MAX_DOCUMENT: usize = 256 * 1024;
const MAX_CORPUS: usize = 16 * 1024 * 1024;
const MAX_CHUNKS: usize = 4096;
const CHUNK_CHARACTERS: usize = 800;
const CHUNK_OVERLAP: usize = 100;
const MAX_DIMENSIONS: usize = 8192;
const MAX_BATCH: usize = 16;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;
const MAX_VECTOR_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EmbeddingConfig {
    pub schema_version: u32,
    pub enabled: bool,
    pub endpoint: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub vector_weight: f64,
    pub keyword_weight: f64,
    pub min_score: Option<f64>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            enabled: false,
            endpoint: String::new(),
            model: String::new(),
            api_key_env: None,
            vector_weight: 0.7,
            keyword_weight: 0.3,
            min_score: None,
        }
    }
}

impl EmbeddingConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("unsupported embedding configuration version".to_owned());
        }
        if !self.endpoint.is_empty() {
            endpoint_url(&self.endpoint)?;
        } else if self.enabled {
            return Err("embedding endpoint is required".to_owned());
        }
        if self.model.len() > 256 || self.model.chars().any(char::is_control) {
            return Err("invalid embedding model name".to_owned());
        }
        if self.enabled && self.model.trim().is_empty() {
            return Err("embedding model is required".to_owned());
        }
        if let Some(name) = &self.api_key_env
            && (name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                || name.as_bytes()[0].is_ascii_digit())
        {
            return Err("invalid embedding API key environment variable name".to_owned());
        }
        if !self.vector_weight.is_finite()
            || !self.keyword_weight.is_finite()
            || self.vector_weight < 0.0
            || self.keyword_weight < 0.0
            || self.vector_weight + self.keyword_weight <= 0.0
            || !(self.vector_weight + self.keyword_weight).is_finite()
        {
            return Err(
                "embedding channel weights must be finite, nonnegative and have a positive sum"
                    .to_owned(),
            );
        }
        if self
            .min_score
            .is_some_and(|score| !score.is_finite() || !(0.0..=1.0).contains(&score))
        {
            return Err("embedding minimum score must be between 0 and 1".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct EmbeddingStatus {
    pub enabled: bool,
    pub configured: bool,
    pub endpoint: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub vector_weight: f64,
    pub keyword_weight: f64,
    pub min_score: Option<f64>,
    pub cache_bytes: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct EmbeddingDocument {
    pub path: String,
    pub hash: String,
    pub content: String,
    pub title: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct VectorHit {
    pub path: String,
    pub title: String,
    pub hash: String,
    pub snippet: String,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub score: f64,
    /// The query variants that ranked the page, by index.
    pub matched_queries: Vec<usize>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct IndexStats {
    pub documents: usize,
    pub chunks: usize,
    pub embedded: usize,
    pub cached: usize,
    pub dimensions: usize,
}

#[derive(Clone)]
struct Chunk {
    key: String,
    input: String,
    hit: VectorHit,
}

struct Indexed {
    cache: cache::Cache,
    namespace: String,
    chunks: Vec<Chunk>,
    vectors: HashMap<String, Vec<f32>>,
    stats: IndexStats,
}

pub(crate) fn load_config(root: &Path) -> Result<EmbeddingConfig, String> {
    let path = root.join(CONFIG_FILE);
    reject_links(&path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(EmbeddingConfig::default()),
        Err(_) => return Err("cannot read embedding configuration".to_owned()),
    };
    let mut text = String::new();
    file.take(u64::try_from(MAX_CONFIG).unwrap_or_default() + 1)
        .read_to_string(&mut text)
        .map_err(|_| "cannot read embedding configuration")?;
    if text.len() > MAX_CONFIG {
        return Err("embedding configuration exceeds its size limit".to_owned());
    }
    let config: EmbeddingConfig =
        serde_json::from_str(&text).map_err(|_| "invalid embedding configuration JSON")?;
    config.validate()?;
    Ok(config)
}

pub(crate) fn save_config(root: &Path, config: &EmbeddingConfig) -> Result<(), String> {
    config.validate()?;
    reject_links(root)?;
    fs::create_dir_all(root).map_err(|_| "cannot create knowledge directory")?;
    let path = root.join(CONFIG_FILE);
    reject_links(&path)?;
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = root.join(format!(
        ".ha-embedding-stage-{}-{suffix}",
        std::process::id()
    ));
    let result = (|| {
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|_| "cannot stage embedding configuration")?;
        let text = serde_json::to_vec_pretty(config)
            .map_err(|_| "cannot encode embedding configuration")?;
        file.write_all(&text)
            .map_err(|_| "cannot write embedding configuration")?;
        file.sync_all()
            .map_err(|_| "cannot persist embedding configuration")?;
        drop(file);
        reject_links(&path)?;
        fs::rename(&staging, path).map_err(|_| "cannot replace embedding configuration")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(staging);
    }
    result
}

pub(crate) fn status(root: &Path) -> Result<EmbeddingStatus, String> {
    let config = load_config(root)?;
    let path = cache_path(root)?;
    let cache_bytes = fs::metadata(path).map_or(0, |metadata| metadata.len());
    Ok(EmbeddingStatus {
        enabled: config.enabled,
        configured: !config.endpoint.is_empty() && !config.model.trim().is_empty(),
        endpoint: config.endpoint,
        model: config.model,
        api_key_env: config.api_key_env,
        vector_weight: config.vector_weight,
        keyword_weight: config.keyword_weight,
        min_score: config.min_score,
        cache_bytes,
    })
}

pub(crate) async fn index(
    root: &Path,
    documents: &[EmbeddingDocument],
) -> Result<IndexStats, String> {
    let config = active_config(root)?;
    let _lock = operation_lock(root).await?;
    Ok(index_inner(root, documents, &config).await?.stats)
}

/// One query's vector search; see [`search_variants`].
#[cfg(test)]
pub(crate) async fn search(
    root: &Path,
    documents: &[EmbeddingDocument],
    query: &str,
    limit: usize,
) -> Result<Vec<VectorHit>, String> {
    search_variants(root, documents, &[query.to_owned()], limit).await
}

/// The pages nearest any query variant, each at its best similarity to one.
/// Every variant is searched, as keyword search searches them, and embedded -
/// the ones not cached in one request: only the first was, and a variant worded
/// the way a page is written found nothing the keyword channel had not.
pub(crate) async fn search_variants(
    root: &Path,
    documents: &[EmbeddingDocument],
    queries: &[String],
    limit: usize,
) -> Result<Vec<VectorHit>, String> {
    let config = active_config(root)?;
    if queries.is_empty()
        || queries.len() > MAX_BATCH
        || queries.iter().any(|query| {
            query.trim().is_empty() || query.len() > 512 || query.chars().any(char::is_control)
        })
    {
        return Err(
            "embedding query must be a nonempty single line of at most 512 bytes".to_owned(),
        );
    }
    if documents.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let _lock = operation_lock(root).await?;
    let mut indexed = index_inner(root, documents, &config).await?;
    let mut vectors = Vec::with_capacity(queries.len());
    for query in queries {
        vectors.push(
            indexed
                .cache
                .query(
                    &indexed.namespace,
                    &hash(query.as_bytes()),
                    indexed.stats.dimensions,
                )
                .await?,
        );
    }
    let mut fetched = request_missing(&config, queries, &mut vectors).await?;
    if vectors
        .iter()
        .flatten()
        .any(|vector| vector.len() != indexed.stats.dimensions)
    {
        // The model changed: the documents are embedded again, and so is every
        // variant whose vector is of the old size.
        indexed.cache.clear(&indexed.namespace).await?;
        indexed = index_inner(root, documents, &config).await?;
        for vector in &mut vectors {
            if vector
                .as_ref()
                .is_some_and(|vector| vector.len() != indexed.stats.dimensions)
            {
                *vector = None;
            }
        }
        fetched.extend(request_missing(&config, queries, &mut vectors).await?);
        if vectors
            .iter()
            .flatten()
            .any(|vector| vector.len() != indexed.stats.dimensions)
        {
            return Err(
                "embedding provider returned inconsistent query and document dimensions".to_owned(),
            );
        }
    }
    for index in fetched {
        if let Some(vector) = &vectors[index] {
            indexed
                .cache
                .put_query(&indexed.namespace, &hash(queries[index].as_bytes()), vector)
                .await?;
        }
    }
    let vectors = vectors.into_iter().flatten().collect::<Vec<_>>();
    fuse_variants(&indexed, &vectors, limit.min(MAX_DOCUMENTS))
}

/// Each page at the passage nearest any variant, the nearest first.
/// `matched_queries` names the variants the page is among the nearest pages of,
/// with a positive similarity. Vector search ranks every page, so a rank alone
/// says nothing: fused by rank, pages no variant was near outranked the one
/// page a variant was (seen with 150 pages in the 0.6.3 live run).
fn fuse_variants(
    indexed: &Indexed,
    vectors: &[Vec<f32>],
    ranked: usize,
) -> Result<Vec<VectorHit>, String> {
    let mut best = BTreeMap::<String, VectorHit>::new();
    for (variant, query_vector) in vectors.iter().enumerate() {
        let mut hits = BTreeMap::<String, VectorHit>::new();
        for chunk in &indexed.chunks {
            let vector = indexed
                .vectors
                .get(&chunk.key)
                .ok_or("embedding cache is incomplete")?;
            let mut hit = chunk.hit.clone();
            hit.score = cosine(query_vector, vector)?;
            if hits
                .get(&hit.path)
                .is_none_or(|existing| hit.score > existing.score)
            {
                hits.insert(hit.path.clone(), hit);
            }
        }
        let mut near = hits
            .values()
            .filter(|hit| hit.score > 0.0)
            .collect::<Vec<_>>();
        near.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.path.cmp(&b.path))
        });
        near.truncate(ranked);
        let near = near
            .into_iter()
            .map(|hit| hit.path.clone())
            .collect::<BTreeSet<_>>();
        for hit in hits.into_values() {
            let found = near.contains(&hit.path);
            let entry = best.entry(hit.path.clone()).or_insert_with(|| hit.clone());
            if hit.score > entry.score {
                let matched = std::mem::take(&mut entry.matched_queries);
                *entry = hit;
                entry.matched_queries = matched;
            }
            if found {
                entry.matched_queries.push(variant);
            }
        }
    }
    let mut hits = best.into_values().collect::<Vec<_>>();
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.path.cmp(&b.path))
    });
    hits.truncate(ranked);
    Ok(hits)
}

/// Embed the variants without a vector, in one request; returns their indexes.
async fn request_missing(
    config: &EmbeddingConfig,
    queries: &[String],
    vectors: &mut [Option<Vec<f32>>],
) -> Result<Vec<usize>, String> {
    let missing = vectors
        .iter()
        .enumerate()
        .filter(|(_, vector)| vector.is_none())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(missing);
    }
    let inputs = missing
        .iter()
        .map(|index| queries[*index].clone())
        .collect::<Vec<_>>();
    let answered = request_vectors(config, &inputs).await?;
    if answered.len() != missing.len() {
        return Err("embedding provider returned the wrong number of vectors".to_owned());
    }
    for (index, vector) in missing.iter().zip(answered) {
        vectors[*index] = Some(vector);
    }
    Ok(missing)
}

pub(crate) fn index_sync(
    root: &Path,
    documents: &[EmbeddingDocument],
) -> Result<IndexStats, String> {
    sync_bridge(|| index(root, documents))
}

pub(crate) fn search_sync(
    root: &Path,
    documents: &[EmbeddingDocument],
    queries: &[String],
    limit: usize,
) -> Result<Vec<VectorHit>, String> {
    sync_bridge(|| async {
        tokio::time::timeout(
            Duration::from_secs(25),
            search_variants(root, documents, queries, limit),
        )
        .await
        .map_err(|_| {
            "semantic search exceeded its time limit; run /knowledge semantic index first"
                .to_owned()
        })?
    })
}

fn sync_bridge<T: Send, F: std::future::Future<Output = Result<T, String>>>(
    create: impl FnOnce() -> F + Send,
) -> Result<T, String> {
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "cannot start embedding runtime")?
                    .block_on(create())
            })
            .join()
            .map_err(|_| "embedding worker stopped unexpectedly")?
    })
}

/// `CowAgent` fuses rank-normalized scores over channels that returned each page.
#[allow(
    clippy::cast_precision_loss,
    reason = "Bounded ranks have at most 2048 entries"
)]
pub(crate) fn hybrid_fuse(
    lexical: &[(String, f64)],
    vectors: &[(String, f64)],
    vector_weight: f64,
    keyword_weight: f64,
    min_score: Option<f64>,
) -> Vec<(String, f64)> {
    if !vector_weight.is_finite()
        || !keyword_weight.is_finite()
        || vector_weight < 0.0
        || keyword_weight < 0.0
        || vector_weight + keyword_weight <= 0.0
    {
        return Vec::new();
    }
    let mut scores = BTreeMap::<String, (f64, f64)>::new();
    for (channel, weight) in [(lexical, keyword_weight), (vectors, vector_weight)] {
        let mut seen = BTreeSet::new();
        let unique = channel
            .iter()
            .filter(|(path, score)| score.is_finite() && seen.insert(path))
            .collect::<Vec<_>>();
        let count = unique.len();
        for (rank, (path, _)) in unique.into_iter().enumerate() {
            let normalized = (count - rank) as f64 / count as f64;
            let entry = scores.entry(path.clone()).or_default();
            entry.0 += normalized * weight;
            entry.1 += weight;
        }
    }
    let minimum = min_score.filter(|score| score.is_finite()).unwrap_or(0.0);
    let mut hits = scores
        .into_iter()
        .filter_map(|(path, (score, weight))| {
            let score = if weight > 0.0 { score / weight } else { 0.0 };
            (weight > 0.0 && score >= minimum).then_some((path, score))
        })
        .collect::<Vec<_>>();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hits
}

async fn index_inner(
    root: &Path,
    documents: &[EmbeddingDocument],
    config: &EmbeddingConfig,
) -> Result<Indexed, String> {
    let chunks = chunk_documents(documents)?;
    let namespace =
        hash(format!("{}\n{}", endpoint_url(&config.endpoint)?, config.model).as_bytes());
    let cache = cache::Cache::open(root).await?;
    cache
        .reconcile(
            &namespace,
            &chunks.iter().map(|chunk| chunk.key.clone()).collect(),
        )
        .await?;
    let (mut dimensions, mut vectors) = cache.load(&namespace).await?;
    let mut cached = vectors.len();
    let mut embedded = 0;
    let mut reset_dimensions = false;
    loop {
        let pending = chunks
            .iter()
            .filter(|chunk| !vectors.contains_key(&chunk.key))
            .take(MAX_BATCH)
            .collect::<Vec<_>>();
        if pending.is_empty() {
            break;
        }
        let response = request_vectors(
            config,
            &pending
                .iter()
                .map(|chunk| chunk.input.clone())
                .collect::<Vec<_>>(),
        )
        .await?;
        let next_dimensions = response[0].len();
        if dimensions != 0 && dimensions != next_dimensions {
            if reset_dimensions || embedded > 0 || cached == 0 {
                return Err(
                    "embedding provider changed vector dimensions within one index operation"
                        .to_owned(),
                );
            }
            cache.clear(&namespace).await?;
            vectors.clear();
            cached = 0;
            reset_dimensions = true;
        }
        dimensions = next_dimensions;
        let entries = pending
            .into_iter()
            .zip(response)
            .map(|(chunk, vector)| (chunk.key.clone(), vector))
            .collect::<Vec<_>>();
        cache.put_vectors(&namespace, dimensions, &entries).await?;
        embedded += entries.len();
        vectors.extend(entries);
    }
    let stats = IndexStats {
        documents: documents.len(),
        chunks: chunks.len(),
        embedded,
        cached,
        dimensions,
    };
    Ok(Indexed {
        cache,
        namespace,
        chunks,
        vectors,
        stats,
    })
}

fn active_config(root: &Path) -> Result<EmbeddingConfig, String> {
    let config = load_config(root)?;
    if !config.enabled {
        return Err("semantic retrieval is disabled".to_owned());
    }
    Ok(config)
}

fn endpoint_url(endpoint: &str) -> Result<Url, String> {
    if endpoint.len() > 2048 {
        return Err("embedding endpoint exceeds its size limit".to_owned());
    }
    let url = Url::parse(endpoint).map_err(|_| "invalid embedding endpoint URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "embedding endpoint must not contain credentials, query parameters or fragments"
                .to_owned(),
        );
    }
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if url.host_str().is_none()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        return Err(
            "embedding endpoint requires HTTPS or an explicit loopback HTTP address".to_owned(),
        );
    }
    Ok(url)
}

async fn request_vectors(
    config: &EmbeddingConfig,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    if inputs.is_empty() || inputs.len() > MAX_BATCH {
        return Err("invalid embedding batch size".to_owned());
    }
    let client = harness_providers::with_extra_roots(Client::builder())
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "cannot initialize embedding HTTP client")?;
    let mut request = client.post(endpoint_url(&config.endpoint)?).json(
        &serde_json::json!({"model": config.model, "input": inputs, "encoding_format": "float"}),
    );
    if let Some(name) = &config.api_key_env {
        let key = std::env::var(name)
            .map_err(|_| "configured embedding API key environment variable is unavailable")?;
        if key.trim().is_empty() || key.len() > 16384 || key.chars().any(char::is_control) {
            return Err(
                "configured embedding API key environment variable is empty or invalid".to_owned(),
            );
        }
        request = request.bearer_auth(key);
    }
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            "embedding provider request timed out".to_owned()
        } else {
            "embedding provider request failed".to_owned()
        }
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "embedding provider returned HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(MAX_RESPONSE).unwrap_or_default())
    {
        return Err("embedding provider response exceeds its size limit".to_owned());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "cannot read embedding provider response")?;
        if body.len() + chunk.len() > MAX_RESPONSE {
            return Err("embedding provider response exceeds its size limit".to_owned());
        }
        body.extend_from_slice(&chunk);
    }
    parse_vectors(&body, inputs.len())
}

#[derive(Deserialize)]
struct Response {
    data: Vec<ResponseEntry>,
}
#[derive(Deserialize)]
struct ResponseEntry {
    index: usize,
    embedding: Vec<f32>,
}

fn parse_vectors(body: &[u8], count: usize) -> Result<Vec<Vec<f32>>, String> {
    let response: Response =
        serde_json::from_slice(body).map_err(|_| "invalid embedding provider JSON response")?;
    if response.data.len() != count {
        return Err("embedding provider returned an incorrect vector count".to_owned());
    }
    let mut output = vec![None; count];
    let mut dimensions = 0;
    for entry in response.data {
        if entry.index >= count || output[entry.index].is_some() {
            return Err(
                "embedding provider returned missing, duplicate or invalid indices".to_owned(),
            );
        }
        validate_vector(&entry.embedding)?;
        if dimensions != 0 && dimensions != entry.embedding.len() {
            return Err("embedding provider returned inconsistent vector dimensions".to_owned());
        }
        dimensions = entry.embedding.len();
        output[entry.index] = Some(entry.embedding);
    }
    output
        .into_iter()
        .map(|vector| vector.ok_or_else(|| "embedding provider omitted an input vector".to_owned()))
        .collect()
}

fn validate_vector(vector: &[f32]) -> Result<(), String> {
    if vector.is_empty()
        || vector.len() > MAX_DIMENSIONS
        || vector.iter().any(|value| !value.is_finite())
    {
        return Err("embedding vector has invalid dimensions or nonfinite values".to_owned());
    }
    if vector.iter().all(|value| *value == 0.0) {
        return Err("embedding vector must not be zero".to_owned());
    }
    Ok(())
}

fn chunk_documents(documents: &[EmbeddingDocument]) -> Result<Vec<Chunk>, String> {
    if documents.len() > MAX_DOCUMENTS {
        return Err("embedding corpus exceeds its page limit".to_owned());
    }
    let mut bytes = 0;
    let mut paths = BTreeSet::new();
    let mut chunks = Vec::new();
    for document in documents {
        bytes += document.content.len();
        if document.content.len() > MAX_DOCUMENT || bytes > MAX_CORPUS {
            return Err("embedding corpus exceeds its byte limit".to_owned());
        }
        if document.path.is_empty()
            || document.path.len() > 1024
            || !paths.insert(&document.path)
            || Path::new(&document.path)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || document.title.len() > 1024
            || document.hash.is_empty()
            || document.hash.len() > 256
        {
            return Err("invalid or duplicate embedding document identity".to_owned());
        }
        chunks.extend(chunk_document(document));
        if chunks.len() > MAX_CHUNKS {
            return Err("embedding corpus exceeds its 4096 chunk limit".to_owned());
        }
    }
    Ok(chunks)
}

fn chunk_document(document: &EmbeddingDocument) -> Vec<Chunk> {
    let mut positions = Vec::new();
    let (mut line, mut column) = (1, 1);
    for (byte, character) in document.content.char_indices() {
        positions.push((byte, line, column));
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    positions.push((document.content.len(), line, column));
    let length = positions.len() - 1;
    let mut start = if document.content.starts_with("<!-- ha-knowledge:") {
        document
            .content
            .find('\n')
            .map_or(length, |byte| document.content[..=byte].chars().count())
    } else {
        0
    };
    let mut chunks = Vec::new();
    while start < length {
        let mut end = (start + CHUNK_CHARACTERS).min(length);
        if end < length
            && let Some(boundary) = (start + CHUNK_CHARACTERS / 2..end)
                .rev()
                .find(|index| document.content.as_bytes()[positions[*index].0] == b'\n')
        {
            end = boundary + 1;
        }
        let snippet = document.content[positions[start].0..positions[end].0].to_owned();
        if !snippet.trim().is_empty() {
            let input = format!("{}\n{snippet}", document.title);
            let key = hash(
                format!(
                    "{}\n{}\n{}\n{}",
                    document.path,
                    document.hash,
                    positions[start].0,
                    hash(input.as_bytes())
                )
                .as_bytes(),
            );
            chunks.push(Chunk {
                key,
                input,
                hit: VectorHit {
                    path: document.path.clone(),
                    title: document.title.clone(),
                    hash: document.hash.clone(),
                    snippet,
                    start_line: positions[start].1,
                    start_column: positions[start].2,
                    end_line: positions[end.saturating_sub(1)].1,
                    score: 0.0,
                    matched_queries: Vec::new(),
                },
            });
        }
        if end == length {
            break;
        }
        start = end.saturating_sub(CHUNK_OVERLAP).max(start + 1);
    }
    chunks
}

fn cosine(left: &[f32], right: &[f32]) -> Result<f64, String> {
    if left.len() != right.len() {
        return Err("embedding query and document dimensions differ".to_owned());
    }
    validate_vector(left)?;
    validate_vector(right)?;
    let (mut dot, mut norm_left, mut norm_right) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (left, right) in left.iter().zip(right) {
        let (left, right) = (f64::from(*left), f64::from(*right));
        dot += left * right;
        norm_left += left * left;
        norm_right += right * right;
    }
    Ok((dot / (norm_left.sqrt() * norm_right.sqrt())).clamp(-1.0, 1.0))
}

fn hash(bytes: &[u8]) -> String {
    harness_types::ContentHash::from_bytes(bytes)
        .as_str()
        .trim_start_matches("sha256:")
        .to_owned()
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn decode_vector(bytes: &[u8], dimensions: usize) -> Result<Vec<f32>, String> {
    if dimensions == 0 || dimensions > MAX_DIMENSIONS || bytes.len() != dimensions * 4 {
        return Err("invalid cached embedding dimensions".to_owned());
    }
    let vector = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect::<Vec<_>>();
    validate_vector(&vector)?;
    Ok(vector)
}

fn cache_path(root: &Path) -> Result<PathBuf, String> {
    let path = root.join(CACHE_FILE);
    reject_links(&path)?;
    for suffix in ["-journal", "-wal", "-shm"] {
        reject_links(&root.join(format!("{CACHE_FILE}{suffix}")))?;
    }
    Ok(path)
}

async fn operation_lock(root: &Path) -> Result<File, String> {
    reject_links(root)?;
    fs::create_dir_all(root).map_err(|_| "cannot create embedding directory")?;
    let path = root.join(".ha-embedding.lock");
    reject_links(&path)?;
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| "cannot open embedding lock")?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err("another embedding operation is still running".to_owned());
            }
            Err(fs::TryLockError::Error(_)) => return Err("cannot lock embedding cache".to_owned()),
        }
    }
}

fn reject_links(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                #[cfg(windows)]
                let reparse = {
                    use std::os::windows::fs::MetadataExt;
                    metadata.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let reparse = false;
                if metadata.file_type().is_symlink() || reparse {
                    return Err(
                        "embedding paths cannot contain symlinks or reparse points".to_owned()
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("cannot inspect embedding cache path".to_owned()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use super::*;

    type Responder = fn(&[String], usize) -> (u16, String);
    static HTTP_FIXTURE_LOCK: Mutex<()> = Mutex::new(());

    struct Fixture {
        _test_guard: std::sync::MutexGuard<'static, ()>,
        endpoint: String,
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl Fixture {
        fn new(respond: Responder) -> Self {
            let test_guard = HTTP_FIXTURE_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let listener = TcpListener::bind("[::1]:0")
                .or_else(|_| TcpListener::bind("127.0.0.1:0"))
                .unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}/v1/embeddings", listener.local_addr().unwrap());
            let calls = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let (worker_calls, worker_stop) = (Arc::clone(&calls), Arc::clone(&stop));
            let worker = std::thread::spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let Some(inputs) = request_inputs(&mut stream) else {
                                continue;
                            };
                            let count = {
                                let mut calls = worker_calls.lock().unwrap();
                                calls.push(inputs.clone());
                                calls.len()
                            };
                            let (status, body) = respond(&inputs, count);
                            let header = format!(
                                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            stream.write_all(header.as_bytes()).unwrap();
                            stream.write_all(body.as_bytes()).unwrap();
                            stream.flush().unwrap();
                            // Wait for the peer to consume the response rather
                            // than resetting a just-written TCP socket on Windows.
                            stream.shutdown(std::net::Shutdown::Write).unwrap();
                            stream
                                .set_read_timeout(Some(Duration::from_millis(500)))
                                .unwrap();
                            let _ = stream.read(&mut [0; 256]);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("fixture accept failed: {error}"),
                    }
                }
            });
            Self {
                _test_guard: test_guard,
                endpoint,
                calls,
                stop,
                worker: Some(worker),
            }
        }

        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        fn configure(&self, root: &Path) {
            save_config(
                root,
                &EmbeddingConfig {
                    enabled: true,
                    endpoint: self.endpoint.clone(),
                    model: "fixture-v1".to_owned(),
                    ..EmbeddingConfig::default()
                },
            )
            .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    fn request_inputs(stream: &mut TcpStream) -> Option<Vec<String>> {
        // Accepted sockets inherit the listener's nonblocking flag on Windows.
        // The fixture uses blocking reads after the nonblocking accept loop.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let (body_start, length) = loop {
            let count = stream.read(&mut buffer).ok()?;
            if count == 0 {
                return None;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if bytes.len() > 128 * 1024 {
                return None;
            }
            if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end]).to_lowercase();
                let length = headers.lines().find_map(|line| {
                    line.strip_prefix("content-length:")?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })?;
                break (header_end + 4, length);
            }
        };
        while bytes.len() - body_start < length {
            let count = stream.read(&mut buffer).ok()?;
            if count == 0 {
                return None;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if bytes.len() > 128 * 1024 {
                return None;
            }
        }
        let request: serde_json::Value =
            serde_json::from_slice(&bytes[body_start..body_start + length]).ok()?;
        serde_json::from_value(request["input"].clone()).ok()
    }

    fn good_response(inputs: &[String], _: usize) -> (u16, String) {
        let data = inputs
            .iter()
            .enumerate()
            .rev()
            .map(|(index, text)| {
                let vector = if text.contains("orchard") {
                    vec![1.0, 0.0, 0.0]
                } else if text.contains("network") {
                    vec![0.0, 1.0, 0.0]
                } else {
                    vec![0.0, 0.0, 1.0]
                };
                serde_json::json!({"index": index, "embedding": vector})
            })
            .collect::<Vec<_>>();
        (200, serde_json::json!({"data": data}).to_string())
    }

    fn document(path: &str, text: &str) -> EmbeddingDocument {
        EmbeddingDocument {
            path: path.to_owned(),
            title: path.to_owned(),
            hash: hash(text.as_bytes()),
            content: text.to_owned(),
        }
    }

    #[test]
    fn unconfigured_and_disabled_semantic_never_create_files_or_contact_a_provider() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("knowledge");
        assert!(!load_config(&root).unwrap().enabled);
        assert!(!status(&root).unwrap().configured);
        assert!(index_sync(&root, &[]).unwrap_err().contains("disabled"));
        assert!(!root.exists());
    }

    #[test]
    fn endpoint_validation_rejects_credentials_queries_and_nonlocal_plaintext() {
        for endpoint in [
            "http://example.com/v1/embeddings",
            "https://user:secret@example.com/v1/embeddings",
            "https://example.com/v1/embeddings?key=secret",
            "file:///embedding",
            "https://example.com/v1/embeddings#fragment",
        ] {
            let error = endpoint_url(endpoint).unwrap_err();
            assert!(!error.contains("secret"));
        }
        for endpoint in [
            "https://example.com/v1/embeddings",
            "http://127.0.0.1:8000/v1/embeddings",
            "http://localhost:8000/embeddings",
            "http://[::1]:8000/embeddings",
        ] {
            assert!(endpoint_url(endpoint).is_ok(), "{endpoint}");
        }
    }

    #[test]
    fn malformed_vector_responses_are_rejected_without_echoing_provider_data() {
        for body in [
            r#"{"data":[]}"#,
            r#"{"data":[{"index":1,"embedding":[1,0]}]}"#,
            r#"{"data":[{"index":0,"embedding":[0,0]}]}"#,
            r#"{"data":[{"index":0,"embedding":[] }]}"#,
            r#"{"data":[{"index":0,"embedding":[1e100,0]}]}"#,
            r#"{"error":"secret-provider-payload"}"#,
        ] {
            let error = parse_vectors(body.as_bytes(), 1).unwrap_err();
            assert!(!error.contains("secret-provider-payload"));
        }
        assert!(
            parse_vectors(
                br#"{"data":[{"index":0,"embedding":[1,0]},{"index":0,"embedding":[1,0]}]}"#,
                2
            )
            .unwrap_err()
            .contains("indices")
        );
        assert!(
            parse_vectors(
                br#"{"data":[{"index":0,"embedding":[1,0]},{"index":1,"embedding":[1,0,0]}]}"#,
                2
            )
            .unwrap_err()
            .contains("dimensions")
        );
        assert!(validate_vector(&[f32::NAN, 1.0]).is_err());
        assert!(validate_vector(&[f32::INFINITY, 1.0]).is_err());
        assert!(validate_vector(&vec![1.0; MAX_DIMENSIONS + 1]).is_err());
    }

    #[test]
    fn unicode_chunk_offsets_preserve_source_lines_and_hide_internal_metadata() {
        let text = format!(
            "<!-- ha-knowledge: {{\"title\":\"private-metadata\"}} -->\n# Orchard\n{}\nnetwork",
            "ế😀".repeat(700)
        );
        let chunks = chunk_documents(&[document("concepts/unicode.md", &text)]).unwrap();
        assert!(chunks.len() > 1);
        assert_eq!(chunks[0].hit.start_line, 2);
        assert!(
            chunks
                .iter()
                .all(|chunk| !chunk.input.contains("private-metadata"))
        );
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.hit.snippet.chars().count() <= CHUNK_CHARACTERS)
        );
        let late = chunks
            .iter()
            .find(|chunk| chunk.hit.start_column > 1)
            .unwrap();
        let original_line = text.lines().nth(late.hit.start_line - 1).unwrap();
        let source_prefix = original_line
            .chars()
            .skip(late.hit.start_column - 1)
            .take(20)
            .collect::<String>();
        assert!(late.hit.snippet.starts_with(&source_prefix));
    }

    #[test]
    fn fusion_matches_cow_rank_normalization_and_preserves_single_channel_hits() {
        let lexical = vec![("shared".to_owned(), 42.0), ("keyword".to_owned(), 1.0)];
        let vectors = vec![("semantic".to_owned(), 0.9), ("shared".to_owned(), 0.8)];
        let fused = hybrid_fuse(&lexical, &vectors, 0.7, 0.3, None);
        assert_eq!(fused[0], ("semantic".to_owned(), 1.0));
        assert!((fused[1].1 - 0.65).abs() < 1e-12);
        assert_eq!(fused[2], ("keyword".to_owned(), 0.5));
        assert_eq!(
            hybrid_fuse(&lexical, &[], 0.7, 0.3, Some(0.6)),
            vec![("shared".to_owned(), 1.0)]
        );
    }

    #[tokio::test]
    async fn real_http_embeddings_cache_queries_and_invalidate_edited_deleted_pages() {
        let fixture = Fixture::new(good_response);
        let root = tempfile::tempdir().unwrap();
        fixture.configure(root.path());
        let mut documents = vec![
            document("concepts/fruit.md", "orchard apples"),
            document("concepts/transport.md", "network TLS"),
        ];
        let initial = index(root.path(), &documents).await.unwrap();
        assert_eq!(
            (initial.embedded, initial.dimensions, fixture.calls()),
            (2, 3, 1)
        );
        let again = index(root.path(), &documents).await.unwrap();
        assert_eq!((again.embedded, again.cached, fixture.calls()), (0, 2, 1));
        let hits = search(root.path(), &documents, "orchard", 10)
            .await
            .unwrap();
        assert_eq!(hits[0].path, "concepts/fruit.md");
        assert_eq!(fixture.calls(), 2);
        search(root.path(), &documents, "orchard", 10)
            .await
            .unwrap();
        assert_eq!(fixture.calls(), 2);
        documents[0] = document("concepts/fruit.md", "orchard pears");
        let updated = index(root.path(), &documents).await.unwrap();
        assert_eq!(
            (updated.embedded, updated.cached, fixture.calls()),
            (1, 1, 3)
        );
        documents.pop();
        let removed = index(root.path(), &documents).await.unwrap();
        assert_eq!(
            (removed.chunks, removed.embedded, fixture.calls()),
            (1, 0, 3)
        );
        let namespace =
            hash(format!("{}\nfixture-v1", endpoint_url(&fixture.endpoint).unwrap()).as_bytes());
        let cache = cache::Cache::open(root.path()).await.unwrap();
        assert_eq!(cache.load(&namespace).await.unwrap().1.len(), 1);
        assert_eq!(
            search(root.path(), &documents, "network", 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            fixture
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|batch| batch.len() <= MAX_BATCH)
        );
    }

    #[tokio::test]
    async fn simultaneous_searches_share_the_persistent_document_and_query_cache() {
        let fixture = Fixture::new(good_response);
        let root = tempfile::tempdir().unwrap();
        fixture.configure(root.path());
        let documents = vec![document("fruit.md", "orchard apples")];
        let (first, second) = tokio::join!(
            search(root.path(), &documents, "orchard", 5),
            search(root.path(), &documents, "orchard", 5)
        );
        assert_eq!(first.unwrap()[0].path, "fruit.md");
        assert_eq!(second.unwrap()[0].path, "fruit.md");
        assert_eq!(
            fixture.calls(),
            2,
            "one document batch and one cached query"
        );
    }

    #[tokio::test]
    async fn model_changes_invalidate_cached_vectors() {
        let fixture = Fixture::new(good_response);
        let root = tempfile::tempdir().unwrap();
        fixture.configure(root.path());
        let documents = vec![document("concepts/fruit.md", "orchard apples")];
        index(root.path(), &documents).await.unwrap();
        let mut config = load_config(root.path()).unwrap();
        config.model = "fixture-v2".to_owned();
        save_config(root.path(), &config).unwrap();
        let result = index(root.path(), &documents).await.unwrap();
        assert_eq!((result.embedded, result.cached, fixture.calls()), (1, 0, 2));
    }

    #[tokio::test]
    async fn changed_dimensions_rebuild_old_vectors_without_reusing_incompatible_cache() {
        fn changing(inputs: &[String], count: usize) -> (u16, String) {
            let dimensions = if count == 1 { 2 } else { 3 };
            let data = inputs.iter().enumerate().map(|(index, _)| serde_json::json!({"index": index, "embedding": vec![1.0; dimensions]})).collect::<Vec<_>>();
            (200, serde_json::json!({"data": data}).to_string())
        }
        let fixture = Fixture::new(changing);
        let root = tempfile::tempdir().unwrap();
        fixture.configure(root.path());
        let mut documents = vec![document("a.md", "orchard"), document("b.md", "network")];
        assert_eq!(index(root.path(), &documents).await.unwrap().dimensions, 2);
        documents[0] = document("a.md", "orchard edited");
        let result = index(root.path(), &documents).await.unwrap();
        assert_eq!(
            (result.dimensions, result.cached, fixture.calls()),
            (3, 0, 3)
        );
        assert_eq!(
            search(root.path(), &documents, "orchard", 2)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn provider_failure_signals_keyword_fallback_without_leaking_response_or_key() {
        fn forbidden(_: &[String], _: usize) -> (u16, String) {
            (
                401,
                "Authorization Bearer super-secret-provider-data".to_owned(),
            )
        }
        let fixture = Fixture::new(forbidden);
        let root = tempfile::tempdir().unwrap();
        fixture.configure(root.path());
        let error = search(root.path(), &[document("a.md", "orchard")], "orchard", 10)
            .await
            .unwrap_err();
        assert_eq!(error, "embedding provider returned HTTP 401");
        assert!(!error.contains("super-secret"));
    }

    #[tokio::test]
    async fn sync_bridge_can_be_called_from_an_existing_tokio_runtime() {
        let fixture = Fixture::new(good_response);
        let root = tempfile::tempdir().unwrap();
        fixture.configure(root.path());
        let hits = search_sync(
            root.path(),
            &[document("a.md", "orchard")],
            &["orchard".to_owned()],
            10,
        )
        .unwrap();
        assert_eq!(hits[0].path, "a.md");
    }

    /// Every query variant is embedded: a page only the second variant is near
    /// is found, with the variant that found it, and the variants not cached
    /// are embedded in one request.
    #[tokio::test]
    async fn every_query_variant_is_embedded_and_fused() {
        let fixture = Fixture::new(good_response);
        let root = tempfile::tempdir().unwrap();
        fixture.configure(root.path());
        let documents = [
            document("a.md", "orchard"),
            document("b.md", "other"),
            document("c.md", "other too"),
            document("z.md", "network"),
        ];
        let first = search(root.path(), &documents, "orchard", 2).await.unwrap();
        assert_eq!(
            first
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>(),
            ["a.md", "b.md"]
        );
        let requests = fixture.calls.lock().unwrap().len();
        let hits = search_variants(
            root.path(),
            &documents,
            &["orchard".to_owned(), "network".to_owned()],
            2,
        )
        .await
        .unwrap();
        let found = hits
            .iter()
            .map(|hit| (hit.path.as_str(), hit.matched_queries.clone()))
            .collect::<Vec<_>>();
        // Each page found by the variant it is near, and only that one.
        assert_eq!(found, [("a.md", vec![0]), ("z.md", vec![1])]);
        let calls = fixture.calls.lock().unwrap().clone();
        assert_eq!(
            calls[requests..],
            [vec!["network".to_owned()]],
            "the cached variant is not asked for again"
        );
    }
}
