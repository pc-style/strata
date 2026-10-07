//! A single-node, explicitly mapped HTTP service. Durable writes and search refreshes are
//! separate operations. This is an Elasticsearch-inspired API, not a compatible replacement.
mod schema;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query as Params, State, rejection::JsonRejection},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
};
use lucene_rs::{
    DirectoryReader, FieldDoc, IndexSearcher, IndexWriter, IndexWriterConfig, Query, Sort,
    SortValue, Term,
};
use schema::{CreateIndex, Mappings, SearchRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, ApiError>;
type Shared = Arc<Service>;
struct Service {
    catalog: Mutex<Catalog>,
    permits: Arc<tokio::sync::Semaphore>,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}
impl ApiError {
    fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    fn missing() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: "not found".into(),
        }
    }
    fn conflict(message: &str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }
    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "storage or worker failure; inspect server logs".into(),
        }
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ApiError {}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error":self.message,"status":self.status.as_u16()})),
        )
            .into_response()
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        eprintln!("storage: {e}");
        Self::internal()
    }
}
impl From<lucene_rs::Error> for ApiError {
    fn from(e: lucene_rs::Error) -> Self {
        match e {
            lucene_rs::Error::IllegalArgument(_) | lucene_rs::Error::QueryParse(_) => {
                Self::bad(e.to_string())
            }
            lucene_rs::Error::LockObtainFailed(_) => Self::conflict("index is locked"),
            _ => {
                eprintln!("engine: {e}");
                Self::internal()
            }
        }
    }
}
fn decode<T: serde::de::DeserializeOwned>(
    body: std::result::Result<Json<T>, JsonRejection>,
) -> Result<T> {
    body.map(|Json(v)| v).map_err(|e| ApiError {
        status: e.status(),
        message: e.body_text(),
    })
}

struct Cursor {
    query: Query,
    sort: Sort,
    generation: u64,
    last: FieldDoc,
    total_hits: u64,
}

struct Index {
    path: PathBuf,
    mappings: Mappings,
    writer: IndexWriter,
    searcher: IndexSearcher,
    cursors: BTreeMap<String, (Instant, Cursor)>,
    failed: bool,
}
impl Index {
    fn open(path: PathBuf, mappings: Mappings) -> Result<Self> {
        mappings.validate()?;
        let writer = IndexWriter::open(&path, IndexWriterConfig::default())?;
        let searcher = IndexSearcher::new(DirectoryReader::open(&path)?);
        Ok(Self {
            path,
            mappings,
            writer,
            searcher,
            cursors: BTreeMap::new(),
            failed: false,
        })
    }
    fn commit(&mut self) -> Result<()> {
        if let Err(e) = self.writer.commit() {
            self.failed = true;
            return Err(e.into());
        }
        Ok(())
    }
    fn refresh(&mut self) -> Result<()> {
        if let Some(reader) = self.searcher.reader().reopen()? {
            self.searcher = IndexSearcher::new(reader);
            self.cursors.clear();
        }
        Ok(())
    }
    fn write(&mut self, id: &str, source: &Value) -> Result<()> {
        let document = self.mappings.document(id, source)?;
        if let Err(e) = self.writer.update_document(Term::new("_id", id), &document) {
            if matches!(e, lucene_rs::Error::Io(_) | lucene_rs::Error::Corrupt(_)) {
                self.failed = true;
            }
            return Err(e.into());
        }
        Ok(())
    }
    fn search(&mut self, request: &SearchRequest) -> Result<Value> {
        if !(1..=10_000).contains(&request.size) {
            return Err(ApiError::bad("page size must be 1..=10000"));
        }
        if request.query.to_string().len() > 16_384 {
            return Err(ApiError::bad("query limit is 16 KiB"));
        }
        let query = self.mappings.query(&request.query, 0)?;
        let sort = self.mappings.sort(&request.sort)?;
        self.cursors
            .retain(|_, (at, _)| at.elapsed() < Duration::from_secs(300));
        let after = request
            .search_after
            .as_ref()
            .map(|token| {
                self.cursors.get(token).map(|(_, c)| c).ok_or_else(|| {
                    ApiError::conflict("cursor expired, refreshed, or belongs to another index")
                })
            })
            .transpose()?;
        if let Some(cursor) = after {
            if cursor.generation != self.searcher.reader().generation() {
                return Err(ApiError::conflict(
                    "cursor belongs to a different search snapshot",
                ));
            }
            if cursor.query != query || cursor.sort != sort {
                return Err(ApiError::bad("cursor belongs to a different query or sort"));
            }
        }
        let mut results = self.searcher.search_after(
            &query,
            &sort,
            request.size.saturating_add(1),
            after.map(|c| &c.last),
        )?;
        let total_hits = match after {
            Some(cursor) => cursor.total_hits,
            None => self.searcher.count(&query)?,
        };
        let more = results.len() > request.size;
        results.truncate(request.size);
        let next = results.last().filter(|_| more).map(|last| Cursor {
            query,
            sort,
            generation: self.searcher.reader().generation(),
            last: last.clone(),
            total_hits,
        });
        let mut hits = Vec::new();
        let mut response_bytes = 0usize;
        for hit in results {
            let doc = self.searcher.doc(hit.doc)?;
            response_bytes =
                response_bytes.saturating_add(doc.get_str("_source").map_or(0, str::len));
            if response_bytes > 16 * 1024 * 1024 {
                return Err(ApiError::bad(
                    "response exceeds 16 MiB; request a smaller page",
                ));
            }
            let source = source(&doc)?;
            let values: Vec<_> = hit
                .sort_values
                .into_iter()
                .map(|v| match v {
                    None => Value::Null,
                    Some(SortValue::I64(v)) => json!(v),
                    Some(SortValue::F64(v)) => json!(v),
                    Some(SortValue::Keyword(v)) => json!(v),
                })
                .collect();
            hits.push(
                json!({"_id":doc.get_str("_id"),"_score":hit.score,"_source":source,"sort":values}),
            );
        }
        let next = next.map(|cursor| {
            if self.cursors.len() >= 256
                && let Some(oldest) = self
                    .cursors
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(key, _)| key.clone())
            {
                self.cursors.remove(&oldest);
            }
            let token = Uuid::new_v4().to_string();
            self.cursors.insert(token.clone(), (Instant::now(), cursor));
            token
        });
        Ok(
            json!({"hits":{"total":{"value":total_hits,"relation":"eq"},"hits":hits},"search_after":next,"generation":self.searcher.reader().generation()}),
        )
    }
}
fn source(doc: &lucene_rs::Document) -> Result<Value> {
    serde_json::from_str(doc.get_str("_source").ok_or_else(ApiError::internal)?)
        .map_err(|_| ApiError::internal())
}

struct Catalog {
    root: PathBuf,
    indexes: BTreeMap<String, Index>,
    _lock: File,
}
impl Catalog {
    fn open(root: &FsPath) -> Result<Self> {
        fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("server.lock"))?;
        lock.try_lock()
            .map_err(|_| ApiError::conflict("data directory is locked by another server"))?;
        let mut indexes = BTreeMap::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(ApiError::bad("symlinks are not allowed in data directory"));
            }
            if !kind.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            } // interrupted, unpublished create
            schema::name(&name)?;
            let config: CreateIndex =
                serde_json::from_slice(&fs::read(entry.path().join("mapping.json"))?)
                    .map_err(|_| ApiError::internal())?;
            indexes.insert(name, Index::open(entry.path(), config.mappings)?);
        }
        Ok(Self {
            root: root.into(),
            indexes,
            _lock: lock,
        })
    }
    fn index(&mut self, name: &str) -> Result<&mut Index> {
        schema::name(name)?;
        let index = self.indexes.get_mut(name).ok_or_else(ApiError::missing)?;
        if index.failed {
            return Err(ApiError::conflict(
                "index stopped after storage error; restart to recover last commit",
            ));
        }
        Ok(index)
    }
    fn create(&mut self, name: &str, config: &CreateIndex) -> Result<Value> {
        schema::name(name)?;
        config.mappings.validate()?;
        if self.indexes.contains_key(name) || self.root.join(name).exists() {
            return Err(ApiError::conflict("index already exists"));
        }
        if self.indexes.len() >= 128 {
            return Err(ApiError::bad("index limit reached (128)"));
        }
        let staging = self.root.join(format!(".{}.pending", Uuid::new_v4()));
        fs::create_dir(&staging)?;
        let mut mapping = File::create(staging.join("mapping.json"))?;
        let data = serde_json::to_vec(config).map_err(|_| ApiError::internal())?;
        mapping.write_all(&data)?;
        mapping.sync_all()?;
        drop(mapping);
        IndexWriter::open(&staging, IndexWriterConfig::default())?.close()?;
        fs::rename(&staging, self.root.join(name))?;
        if let Ok(directory) = File::open(&self.root) {
            let _ = directory.sync_all();
        }
        self.indexes.insert(
            name.into(),
            Index::open(self.root.join(name), config.mappings.clone())?,
        );
        Ok(json!({"acknowledged":true,"index":name}))
    }
}

async fn work(
    state: Shared,
    action: impl FnOnce(&mut Catalog) -> Result<Value> + Send + 'static,
) -> Result<Json<Value>> {
    let permit = state
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: "server busy; retry later".into(),
        })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut catalog = state.catalog.lock().map_err(|_| ApiError::internal())?;
        action(&mut catalog).map(Json)
    })
    .await
    .map_err(|_| ApiError::internal())?
}

/// Opens the data directory and builds the HTTP service. One process owns a data directory.
/// # Errors
/// Invalid mappings, unreadable storage, or an already-held directory lock.
pub fn router(data: impl AsRef<FsPath>) -> Result<Router> {
    let state = Arc::new(Service {
        catalog: Mutex::new(Catalog::open(data.as_ref())?),
        permits: Arc::new(tokio::sync::Semaphore::new(32)),
    });
    Ok(Router::new()
        .route("/", get(|| async { Json(json!({"name":"strata","version":env!("CARGO_PKG_VERSION"),"compatibility":"Elasticsearch-inspired, not API-compatible"})) }))
        .route("/_health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/_dashboard", get(|| async { (
            [("cache-control", "no-cache"), ("x-content-type-options", "nosniff"), ("content-security-policy", "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; font-src data:; connect-src 'self'; img-src data:; base-uri 'none'; frame-ancestors 'none'; form-action 'self'")],
            Html(include_str!("../dashboard/dist/index.html")),
        ) }))
        .route("/_indexes", get(indexes))
        .route("/{index}", put(create).get(mapping))
        .route("/{index}/_doc/{id}", put(index_document).get(get_document).delete(delete_document))
        .route("/{index}/_refresh", post(refresh))
        .route("/{index}/_search", post(search))
        .route("/{index}/_bulk", post(bulk))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(state))
}

async fn indexes(State(state): State<Shared>) -> Result<Json<Value>> {
    work(state, |catalog| {
        let indexes: Vec<_> = catalog
            .indexes
            .iter()
            .map(|(name, index)| {
                let reader = index.searcher.reader();
                json!({"name":name,"documents":reader.num_docs(),
                "deleted_documents":reader.num_deleted_docs(),"segments":reader.segments().len(),
                "generation":reader.generation(),"failed":index.failed,"mappings":index.mappings})
            })
            .collect();
        Ok(json!({"indexes":indexes}))
    })
    .await
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteOptions {
    #[serde(default)]
    refresh: bool,
}

async fn create(
    State(state): State<Shared>,
    Path(index): Path<String>,
    body: std::result::Result<Json<CreateIndex>, JsonRejection>,
) -> Result<Json<Value>> {
    let config = decode(body)?;
    work(state, move |catalog| catalog.create(&index, &config)).await
}
async fn mapping(State(state): State<Shared>, Path(index): Path<String>) -> Result<Json<Value>> {
    work(state, move |catalog| {
        Ok(json!({"mappings":catalog.index(&index)?.mappings}))
    })
    .await
}
async fn index_document(
    State(state): State<Shared>,
    Path((index, id)): Path<(String, String)>,
    Params(options): Params<WriteOptions>,
    body: std::result::Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>> {
    let document = decode(body)?;
    work(state, move |catalog| {
        let index = catalog.index(&index)?;
        index.write(&id, &document)?;
        index.commit()?;
        if options.refresh {
            index.refresh()?;
        }
        Ok(json!({"_id":id,"result":"indexed","durable":true,"refreshed":options.refresh}))
    })
    .await
}
async fn get_document(
    State(state): State<Shared>,
    Path((index, id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    work(state, move |catalog| {
        let index = catalog.index(&index)?;
        let reader = IndexSearcher::new(DirectoryReader::open(&index.path)?);
        let hit = reader
            .search(&Query::term("_id", &id), 1)?
            .score_docs
            .into_iter()
            .next()
            .ok_or_else(ApiError::missing)?;
        Ok(json!({"_id":id,"found":true,"_source":source(&reader.doc(hit.doc)?)?}))
    })
    .await
}
async fn delete_document(
    State(state): State<Shared>,
    Path((index, id)): Path<(String, String)>,
    Params(options): Params<WriteOptions>,
) -> Result<Json<Value>> {
    work(state, move |catalog| {
        let index = catalog.index(&index)?;
        let reader = IndexSearcher::new(DirectoryReader::open(&index.path)?);
        if reader.count(&Query::term("_id", &id))? == 0 {
            return Err(ApiError::missing());
        }
        index.writer.delete_documents(Term::new("_id", &id));
        index.commit()?;
        if options.refresh {
            index.refresh()?;
        }
        Ok(json!({"_id":id,"result":"deleted","durable":true,"refreshed":options.refresh}))
    })
    .await
}
async fn refresh(State(state): State<Shared>, Path(index): Path<String>) -> Result<Json<Value>> {
    work(state, move |catalog| {
        let index = catalog.index(&index)?;
        index.refresh()?;
        Ok(json!({"refreshed":true,"generation":index.searcher.reader().generation()}))
    })
    .await
}
async fn search(
    State(state): State<Shared>,
    Path(index): Path<String>,
    body: std::result::Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Json<Value>> {
    let request = decode(body)?;
    work(state, move |catalog| {
        catalog.index(&index)?.search(&request)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BulkMetadata {
    #[serde(rename = "_id")]
    id: String,
}
enum BulkOp {
    Index(String, Value),
    Delete(String),
}
fn parse_bulk(body: &str) -> Result<Vec<BulkOp>> {
    let mut lines = body.lines();
    let mut operations = Vec::new();
    while let Some(line) = lines.next() {
        let meta: BTreeMap<String, BulkMetadata> =
            serde_json::from_str(line).map_err(|e| ApiError::bad(e.to_string()))?;
        if meta.len() != 1 {
            return Err(ApiError::bad("bulk action requires exactly one key"));
        }
        for (action, meta) in meta {
            if meta.id.is_empty() || meta.id.len() > 512 {
                return Err(ApiError::bad("invalid bulk ID"));
            }
            operations.push(match action.as_str() {
                "index" => BulkOp::Index(
                    meta.id,
                    serde_json::from_str(
                        lines
                            .next()
                            .ok_or_else(|| ApiError::bad("missing bulk document"))?,
                    )
                    .map_err(|e| ApiError::bad(e.to_string()))?,
                ),
                "delete" => BulkOp::Delete(meta.id),
                _ => return Err(ApiError::bad("bulk supports index and delete")),
            });
        }
        if operations.len() > 1000 {
            return Err(ApiError::bad("bulk limit is 1000 operations"));
        }
    }
    if operations.is_empty() {
        return Err(ApiError::bad("empty bulk request"));
    }
    Ok(operations)
}
async fn bulk(
    State(state): State<Shared>,
    Path(index): Path<String>,
    Params(options): Params<WriteOptions>,
    body: String,
) -> Result<Json<Value>> {
    let operations = parse_bulk(&body)?;
    work(state, move |catalog| {
        let index = catalog.index(&index)?;
        let mut items = Vec::new();
        let mut errors = false;
        for op in operations {
            let (action, id, result) = match op {
                BulkOp::Index(id, document) => {
                    let result = index.write(&id, &document);
                    ("index", id, result)
                }
                BulkOp::Delete(id) => {
                    index.writer.delete_documents(Term::new("_id", &id));
                    ("delete", id, Ok(()))
                }
            };
            match result {
                Ok(()) => items.push(json!({action:{"_id":id,"status":200}})),
                Err(e) if e.status == StatusCode::BAD_REQUEST => {
                    errors = true;
                    items.push(json!({action:{"_id":id,"status":400,"error":e.message}}));
                }
                Err(e) => return Err(e),
            }
        }
        index.commit()?;
        if options.refresh {
            index.refresh()?;
        }
        Ok(json!({"errors":errors,"items":items,"durable":true,"refreshed":options.refresh}))
    })
    .await
}
