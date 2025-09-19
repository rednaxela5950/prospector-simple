use std::{collections::HashMap, env, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use axum::middleware::{from_fn, Next};
use axum::{
    extract::{DefaultBodyLimit, Multipart, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
#[cfg(all(not(test), feature = "p2p_notify"))]
use iroh::protocol::Router as IrohRouter;
use iroh::Endpoint;
use iroh_base::{NodeAddr, PublicKey};
use iroh_blobs::api::downloader::{DownloadProgessItem, DownloadRequest, Shuffled, SplitStrategy};
use iroh_blobs::protocol::GetRequest;
use iroh_blobs::{store::fs::FsStore, BlobsProtocol};
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::Mutex, time::sleep};
use tokio_util::io::ReaderStream;
use tower_http::cors::CorsLayer;
use tracing::{error, info, warn};

mod notify;
use notify::{send_notify, NotifyMsg};
mod chunk_strategy;

/// Shared runtime state for the node.
///
/// Why: centralizes access to the iroh endpoint, blob protocol, persistent store,
/// and the HTTP-visible `NodeState`. `NodeState` is wrapped in a `tokio::sync::Mutex`
/// so concurrent async tasks (HTTP handlers, download progress loop, timers) can
/// safely read/write status. Keep lock sections short to avoid contention.
#[derive(Clone, Debug)]
pub struct NodeShared {
    pub endpoint: Endpoint,
    pub blobs: BlobsProtocol,
    pub store: Arc<FsStore>,
    state: Arc<Mutex<NodeState>>, // for HTTP reporting
    pub data_dir: PathBuf,
    pub peers_http: Vec<String>,
    pub peers_addrs: Arc<Mutex<HashMap<String, NodeAddr>>>, // url -> NodeAddr
    pub latency_min: u64,
    pub latency_max: u64,
    pub stream_sleep_ms: u64,
}

/// Middleware: add Access-Control-Allow-Private-Network for PNA preflights from secure contexts
async fn add_pna_header(req: axum::http::Request<axum::body::Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut().insert(
        "Access-Control-Allow-Private-Network",
        HeaderValue::from_static("true"),
    );
    res
}

/// Status exposed at `GET /status`.
///
/// Invariant: `has_image == true` only after the blob has been FULLY received
/// and exported to `current.img` on disk. This guarantees `/image` and
/// `/image_stream` can immediately serve the completed file when the UI sees
/// `has_image: true`.
#[derive(Debug, Default, Serialize, Clone)]
struct NodeState {
    node_name: String,
    node_addr: Option<String>,
    has_image: bool,
    current_filename: Option<String>,
    content_type: Option<String>,
    current_hash: Option<String>,
    bytes_total: Option<u64>,
    bytes_received: u64,
    progress: f32,
    stripe_providers: HashMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct StatusPeerResp {
    node_addr: Option<String>,
}

#[derive(Deserialize)]
struct ReceiveBody {
    ticket: Option<String>,
    hash: Option<String>,
    filename: String,
    content_type: String,
    provider_node_id: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let node_name = env::var("NODE_NAME").unwrap_or_else(|_| "node".into());
    let http_port: u16 = env::var("HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let data_dir = PathBuf::from(env::var("DATA_DIR").unwrap_or_else(|_| "/data".into()));
    let enable_local =
        env::var("ENABLE_LOCAL_DISCOVERY").unwrap_or_else(|_| "true".into()) == "true";
    let peers_http: Vec<String> = env::var("PEER_HTTP_URLS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .collect();
    let latency_min: u64 = env::var("LATENCY_MS_MIN")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    let latency_max: u64 = env::var("LATENCY_MS_MAX")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(latency_min);
    let stream_sleep_ms: u64 = env::var("STREAM_SLEEP_MS")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(30);

    // Early stdout message to confirm the binary actually starts and to help diagnose container exits.
    println!(
        "p2p-node: starting node '{}' on 0.0.0.0:{} (data_dir={})",
        node_name,
        http_port,
        data_dir.display()
    );

    fs::create_dir_all(&data_dir).await.ok();

    // --- Build iroh endpoint ---
    let mut builder = Endpoint::builder();
    if enable_local {
        builder = builder.discovery_local_network();
    }
    let endpoint = builder.bind().await?;

    // --- iroh-blobs with FS store ---
    let store = Arc::new(FsStore::load(data_dir.join("blobs")).await?);
    let blobs = BlobsProtocol::new(&*store, endpoint.clone(), None);

    // We expose our node id string in status (peers convert to NodeAddr via discovery)
    let node_id = endpoint.node_id();

    let shared = Arc::new(NodeShared {
        endpoint: endpoint.clone(),
        blobs: blobs.clone(),
        store: store.clone(),
        state: Arc::new(Mutex::new(NodeState {
            node_name: node_name.clone(),
            node_addr: Some(node_id.to_string()),
            ..Default::default()
        })),
        data_dir: data_dir.clone(),
        peers_http,
        peers_addrs: Arc::new(Mutex::new(HashMap::new())),
        latency_min,
        latency_max,
        stream_sleep_ms,
    });

    // Router: serve blobs + our custom notify protocol
    #[cfg(all(not(test), feature = "p2p_notify"))]
    let _iroh_router = IrohRouter::builder(endpoint.clone())
        .accept(iroh_blobs::ALPN, blobs.clone())
        .accept(
            notify::NOTIFY_ALPN,
            Arc::new(notify::NotifyHandler {
                shared: shared.clone(),
            }),
        )
        .spawn();

    // Start peer discovery (learn NodeAddrs via peers' /status)
    tokio::spawn(peer_addr_refresher(shared.clone()));

    // --- HTTP server ---
    let app = Router::new()
        .route("/status", get(status))
        .route("/image", get(get_image))
        .route("/image_stream", get(image_stream))
        .route("/upload", post(upload))
        .route("/receive", post(receive_http))
        // Allow uploads up to 20 MiB (adjust as needed)
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        // Add PNA header for HTTPS->localhost CORS preflights
        .layer(from_fn(add_pna_header))
        .with_state(shared.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], http_port));
    info!(%addr, %node_name, "HTTP listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    // Clean shutdown
    // iroh_router.shutdown().await.ok();
    Ok(())
}

async fn status(State(shared): State<Arc<NodeShared>>) -> impl IntoResponse {
    Json(shared.state.lock().await.clone())
}

async fn get_image(State(shared): State<Arc<NodeShared>>) -> impl IntoResponse {
    let mut resp = match fs::read(shared.data_dir.join("current.img")).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .body(bytes.into())
            .unwrap(),
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::empty())
            .unwrap(),
    };
    resp.headers_mut()
        .insert("Access-Control-Allow-Origin", HeaderValue::from_static("*"));
    if let Some(ct) = &shared.state.lock().await.content_type {
        resp.headers_mut().insert(
            "Content-Type",
            HeaderValue::from_str(ct)
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        );
    }
    resp
}

/// Stream the image in chunks with tiny sleeps to encourage progressive rendering in browsers
async fn image_stream(State(shared): State<Arc<NodeShared>>) -> impl IntoResponse {
    let path = shared.data_dir.join("current.img");
    match tokio::fs::File::open(path).await {
        Ok(file) => {
            let delay = shared.stream_sleep_ms;
            let stream = ReaderStream::new(file).then(move |res| {
                let d = delay;
                async move {
                    if d > 0 {
                        sleep(Duration::from_millis(d)).await;
                    }
                    res
                }
            });
            let mut resp = Response::builder()
                .status(StatusCode::OK)
                .body(axum::body::Body::from_stream(stream))
                .unwrap();
            resp.headers_mut()
                .insert("Access-Control-Allow-Origin", HeaderValue::from_static("*"));
            if let Some(ct) = &shared.state.lock().await.content_type {
                resp.headers_mut().insert(
                    "Content-Type",
                    HeaderValue::from_str(ct)
                        .unwrap_or(HeaderValue::from_static("application/octet-stream")),
                );
            }
            resp
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Accepts a multipart file upload, writes it into the local blobs store and
/// saves a copy to `current.img` for HTTP serving. On a provider node the
/// upload is a one-shot write (not a P2P download), so we set `bytes_total`
/// and `bytes_received` to the full size and mark `progress = 100`.
///
/// Also fans out a hash-only notify to peers so they can discover and download.
async fn upload(State(shared): State<Arc<NodeShared>>, mut mp: Multipart) -> impl IntoResponse {
    maybe_latency(&shared).await;

    let mut filename = "upload".to_string();
    let mut content_type = "application/octet-stream".to_string();
    let mut bytes = Vec::new();

    info!("/upload: reading multipart fields");
    while let Ok(Some(mut field)) = mp.next_field().await {
        let field_name = field.name().map(|s| s.to_string());
        let fname_dbg = field.file_name().map(|s| s.to_string());
        info!(?field_name, ?fname_dbg, "multipart field");
        // Prefer the 'file' part; if no name is provided, assume it's the file
        if field_name.as_deref() == Some("file") || field_name.is_none() {
            if let Some(name) = field.file_name().map(|s| s.to_string()) {
                filename = name;
            }
            if let Some(ct) = field.content_type().map(|s| s.to_string()) {
                content_type = ct;
            }
            // Read the file in chunks to avoid surprises if a single read fails
            while let Ok(Some(chunk)) = field.chunk().await {
                bytes.extend_from_slice(&chunk);
                // Safety guard: hard cap at ~50 MiB in this handler even if body limit is higher
                if bytes.len() > 50 * 1024 * 1024 {
                    return (StatusCode::PAYLOAD_TOO_LARGE, "file too large").into_response();
                }
            }
            break;
        }
    }

    info!(
        ?filename,
        ?content_type,
        size = bytes.len(),
        "/upload: parsed file"
    );

    if bytes.is_empty() {
        return (StatusCode::BAD_REQUEST, "no file").into_response();
    }

    // Add to blobs store (track total bytes)
    let total = bytes.len() as u64;
    let tag = shared.blobs.add_slice(&bytes).await.unwrap();
    let ticket = shared.blobs.ticket(tag).await.unwrap();

    // Save a local copy for HTTP serving
    let path = shared.data_dir.join("current.img");
    if let Err(e) = fs::write(&path, &bytes).await {
        error!(?e, "write failed");
    }

    let provider = shared.endpoint.node_id().to_string();

    {
        let mut s = shared.state.lock().await;
        s.has_image = true;
        s.current_filename = Some(filename.clone());
        s.content_type = Some(content_type.clone());
        s.bytes_total = Some(total);
        s.bytes_received = total; // uploader is complete
        s.progress = 100.0;
        s.current_hash = Some(ticket.hash().to_string());
        s.stripe_providers = HashMap::from([(provider.clone(), vec!["all".to_string()])]);
    }

    // P2P notify peers over iroh (fallback to HTTP /receive if unknown) using hash-only model
    let msg = NotifyMsg {
        hash: ticket.hash().to_string(),
        filename: filename.clone(),
        content_type: content_type.clone(),
        provider_node_id: Some(provider.clone()),
    };
    tokio::spawn(notify_all_peers(shared.clone(), msg.clone()));

    Json(serde_json::json!({
        "ticket": ticket.to_string(),
        "hash": ticket.hash().to_string(),
        "filename": filename,
        "content_type": content_type,
        "provider_node_id": provider,
    }))
    .into_response()
}

/// HTTP receive endpoint accepts either a full ticket or just a hash
async fn receive_http(
    State(shared): State<Arc<NodeShared>>,
    Json(msg): Json<ReceiveBody>,
) -> impl IntoResponse {
    maybe_latency(&shared).await;
    if let Some(tk) = msg.ticket {
        match tk.parse::<iroh_blobs::ticket::BlobTicket>() {
            Ok(ticket) => {
                let hash = ticket.hash();
                let fallback = ticket.node_addr().clone();
                if let Err(e) = shared
                    .receive_by_discovery(hash, msg.filename, msg.content_type, Some(fallback))
                    .await
                {
                    error!(?e, "receive (ticket) error");
                    return StatusCode::BAD_GATEWAY.into_response();
                }
                StatusCode::OK.into_response()
            }
            Err(_) => StatusCode::BAD_REQUEST.into_response(),
        }
    } else if let Some(hs) = msg.hash {
        match hs.parse() {
            Ok(hash) => {
                let fallback = msg
                    .provider_node_id
                    .and_then(|s| s.parse::<PublicKey>().ok())
                    .map(NodeAddr::from);
                if let Err(e) = shared
                    .receive_by_discovery(hash, msg.filename, msg.content_type, fallback)
                    .await
                {
                    error!(?e, "receive (hash) error");
                    return StatusCode::BAD_GATEWAY.into_response();
                }
                StatusCode::OK.into_response()
            }
            Err(_) => StatusCode::BAD_REQUEST.into_response(),
        }
    } else {
        StatusCode::BAD_REQUEST.into_response()
    }
}

impl NodeShared {
    /// Discover a provider for the given hash among known peers and download.
    pub async fn receive_by_discovery(
        &self,
        hash: iroh_blobs::Hash,
        filename: String,
        content_type: String,
        fallback: Option<NodeAddr>,
    ) -> anyhow::Result<()> {
        // Initialize state for this transfer
        {
            let mut s = self.state.lock().await;
            s.current_filename = Some(filename.clone());
            s.content_type = Some(content_type.clone());
            s.current_hash = Some(hash.to_string());
            s.has_image = false;
            s.bytes_received = 0;
            s.bytes_total = None;
            s.progress = 0.0;
            s.stripe_providers.clear();
        }

        let downloader = self.store.downloader(&self.endpoint);

        // Build candidate node list from known peers; include fallback if provided
        let mut candidate_addrs: Vec<NodeAddr> = {
            let map = self.peers_addrs.lock().await;
            map.values().cloned().collect()
        };
        if let Some(na) = fallback.as_ref() {
            if !candidate_addrs
                .iter()
                .any(|addr| addr.node_id == na.node_id)
            {
                candidate_addrs.push(na.clone());
            }
        }

        // Register addresses with the endpoint and extract node ids
        let mut candidate_nodes: Vec<iroh_base::PublicKey> = Vec::new();
        for addr in &candidate_addrs {
            if let Err(e) = self.endpoint.add_node_addr(addr.clone()) {
                warn!(?e, "failed to add node addr");
            }
            if !candidate_nodes.iter().any(|pk| *pk == addr.node_id) {
                candidate_nodes.push(addr.node_id);
            }
        }

        if !candidate_nodes.is_empty() {
            match self
                .attempt_split_download(hash, &filename, &content_type, candidate_nodes.clone())
                .await
            {
                Ok(_) => return Ok(()),
                Err(err) => {
                    warn!(
                        ?err,
                        "stripe download failed; falling back to sequential download"
                    );
                    let mut s = self.state.lock().await;
                    s.bytes_received = 0;
                    s.bytes_total = None;
                    s.progress = 0.0;
                    s.stripe_providers.clear();
                }
            }
        }

        let mut last_err: Option<anyhow::Error> = None;
        for addr in candidate_addrs {
            let node_id = addr.node_id;
            let mut last_provider: Option<String> = None;

            // Start the download and obtain a progress stream
            let dl = downloader.download(hash, Some(node_id));
            let mut stream = match dl.stream().await {
                Ok(s) => s,
                Err(e) => {
                    last_err = Some(e.into());
                    continue;
                }
            };

            let mut failed = false;
            while let Some(item) = stream.next().await {
                match item {
                    DownloadProgessItem::Progress(recvd) => {
                        let mut s = self.state.lock().await;
                        s.bytes_received = recvd;
                        if let Some(t) = s.bytes_total {
                            if t > 0 {
                                s.progress = (recvd as f32 / t as f32) * 100.0;
                            }
                        }
                    }
                    DownloadProgessItem::TryProvider { id, .. } => {
                        last_provider = Some(id.to_string());
                    }
                    DownloadProgessItem::ProviderFailed { .. } => {}
                    DownloadProgessItem::PartComplete { .. } => {}
                    DownloadProgessItem::Error(e) => {
                        failed = true;
                        last_err = Some(e);
                        break;
                    }
                    DownloadProgessItem::DownloadError => {
                        failed = true;
                        last_err = Some(anyhow::anyhow!("download error"));
                        break;
                    }
                }
            }

            if failed {
                continue;
            }

            // Export the downloaded blob to our HTTP-served location
            let out_path = self.data_dir.join("current.img");
            let _ = self.store.blobs().export(hash, &out_path).await;
            {
                let mut s = self.state.lock().await;
                let recvd = s.bytes_received;
                s.bytes_total = Some(recvd);
                s.has_image = true;
                s.current_filename = Some(filename.clone());
                s.content_type = Some(content_type.clone());
                s.progress = 100.0;
                if let Some(provider) = last_provider {
                    s.stripe_providers
                        .entry(provider)
                        .or_insert_with(|| vec!["all".to_string()]);
                }
                let self_id = self.endpoint.node_id().to_string();
                let entry = s.stripe_providers.entry(self_id).or_default();
                if !entry.iter().any(|v| v == "all") {
                    entry.push("all".to_string());
                }
            }
            return Ok(());
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no provider found for hash")))
    }

    async fn attempt_split_download(
        &self,
        hash: iroh_blobs::Hash,
        filename: &str,
        content_type: &str,
        providers: Vec<iroh_base::PublicKey>,
    ) -> anyhow::Result<()> {
        if providers.is_empty() {
            return Err(anyhow::anyhow!("no providers supplied for split download"));
        }

        let downloader = self.store.downloader(&self.endpoint);
        let opts = DownloadRequest::new(hash, Shuffled::new(providers), SplitStrategy::Split);
        let mut stream = downloader.download_with_opts(opts).stream().await?;

        let mut owner_for_request: HashMap<String, String> = HashMap::new();
        let mut label_cache: HashMap<String, String> = HashMap::new();

        while let Some(item) = stream.next().await {
            match item {
                DownloadProgessItem::Progress(recvd) => {
                    let mut s = self.state.lock().await;
                    s.bytes_received = recvd;
                    if let Some(t) = s.bytes_total {
                        if t > 0 {
                            s.progress = (recvd as f32 / t as f32) * 100.0;
                        }
                    }
                }
                DownloadProgessItem::TryProvider { id, request } => {
                    let key = request_key(request.as_ref());
                    owner_for_request.insert(key.clone(), id.to_string());
                    label_cache
                        .entry(key)
                        .or_insert_with(|| describe_request(request.as_ref()));
                }
                DownloadProgessItem::ProviderFailed { request, .. } => {
                    owner_for_request.remove(&request_key(request.as_ref()));
                }
                DownloadProgessItem::PartComplete { request } => {
                    let key = request_key(request.as_ref());
                    if let Some(provider) = owner_for_request.get(&key).cloned() {
                        let label = label_cache
                            .entry(key)
                            .or_insert_with(|| describe_request(request.as_ref()))
                            .clone();
                        let mut s = self.state.lock().await;
                        let entry = s.stripe_providers.entry(provider).or_default();
                        if !entry.iter().any(|v| v == &label) {
                            entry.push(label);
                        }
                    }
                }
                DownloadProgessItem::Error(e) => return Err(e),
                DownloadProgessItem::DownloadError => {
                    return Err(anyhow::anyhow!("download error"));
                }
            }
        }

        let out_path = self.data_dir.join("current.img");
        self.store.blobs().export(hash, &out_path).await?;
        {
            let mut s = self.state.lock().await;
            let recvd = s.bytes_received;
            s.bytes_total = Some(recvd);
            s.has_image = true;
            s.current_filename = Some(filename.to_string());
            s.content_type = Some(content_type.to_string());
            s.progress = 100.0;
            let self_id = self.endpoint.node_id().to_string();
            let entry = s.stripe_providers.entry(self_id).or_default();
            if !entry.iter().any(|v| v == "all") {
                entry.push("all".to_string());
            }
        }
        Ok(())
    }
    pub async fn finish_download(
        &self,
        bytes: Vec<u8>,
        filename: &str,
        content_type: &str,
    ) -> anyhow::Result<()> {
        let path = self.data_dir.join("current.img");
        fs::write(&path, bytes).await?;
        let mut s = self.state.lock().await;
        s.has_image = true;
        s.current_filename = Some(filename.to_string());
        s.content_type = Some(content_type.to_string());
        s.progress = 100.0;
        Ok(())
    }

    /// Download using the ticket and update progress fields as chunks arrive
    pub async fn receive_with_progress(
        &self,
        ticket: iroh_blobs::ticket::BlobTicket,
        filename: String,
        content_type: String,
    ) -> anyhow::Result<()> {
        let hash = ticket.hash();
        let node_addr: NodeAddr = ticket.node_addr().clone();

        {
            let mut s = self.state.lock().await;
            s.current_filename = Some(filename.clone());
            s.content_type = Some(content_type.clone());
            s.current_hash = Some(hash.to_string());
            s.has_image = false;
            s.bytes_received = 0;
            s.bytes_total = None; // unknown until we know
            s.progress = 0.0;
            s.stripe_providers.clear();
        }

        // Start the download via the store downloader (iroh-blobs 0.93) and stream progress updates
        let downloader = self.store.downloader(&self.endpoint);
        let dl = downloader.download(hash, Some(node_addr.node_id));
        let mut stream = match dl.stream().await {
            Ok(s) => s,
            Err(e) => {
                return Err(e.into());
            }
        };

        while let Some(item) = stream.next().await {
            match item {
                DownloadProgessItem::Progress(recvd) => {
                    let mut s = self.state.lock().await;
                    s.bytes_received = recvd;
                    if let Some(t) = s.bytes_total {
                        if t > 0 {
                            s.progress = (recvd as f32 / t as f32) * 100.0;
                        }
                    }
                }
                DownloadProgessItem::TryProvider { .. } => {}
                DownloadProgessItem::ProviderFailed { .. } => {}
                DownloadProgessItem::PartComplete { .. } => {}
                DownloadProgessItem::Error(e) => {
                    return Err(e.into());
                }
                DownloadProgessItem::DownloadError => {
                    return Err(anyhow::anyhow!("download error"));
                }
            }
        }

        // Export the downloaded blob to our HTTP-served location
        let out_path = self.data_dir.join("current.img");
        let _ = self.store.blobs().export(hash, &out_path).await;
        // Mark as complete in state
        {
            let mut s = self.state.lock().await;
            let recvd = s.bytes_received;
            s.bytes_total = Some(recvd);
            s.has_image = true;
            s.current_filename = Some(filename);
            s.content_type = Some(content_type);
            s.progress = 100.0;
            s.stripe_providers
                .entry(node_addr.node_id.to_string())
                .or_insert_with(|| vec!["all".to_string()]);
            let self_id = self.endpoint.node_id().to_string();
            let entry = s.stripe_providers.entry(self_id).or_default();
            if !entry.iter().any(|v| v == "all") {
                entry.push("all".to_string());
            }
        }
        Ok(())
    }
}

fn request_key(req: &GetRequest) -> String {
    format!("{}::{:?}", req.hash, req.ranges)
}

fn describe_request(req: &GetRequest) -> String {
    if let Some((offset, ranges)) = req.ranges.as_single() {
        format!("offset={} ranges={:?}", offset, ranges)
    } else {
        format!("ranges={:?}", req.ranges)
    }
}

/// Best‑effort extractor for bytes from a generic progress event (MVP; tolerant to API changes)
#[allow(dead_code)]
fn progress_bytes(evt: &impl core::fmt::Debug) -> Option<(u64, Option<u64>)> {
    let s = format!("{:?}", evt);
    let nums: Vec<u64> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|t| t.parse().ok())
        .collect();
    let recvd = nums.get(0).copied();
    let total = nums.get(1).copied();
    recvd.map(|r| (r, total))
}

/// Best-effort fan-out to peers about a new blob hash.
///
/// First attempts P2P notify via iroh using any known `NodeAddr`s. If the
/// address book is empty or a send fails, falls back to HTTP `/receive`.
/// Why: ensures reliability during early boot or partial discovery.
async fn notify_all_peers(shared: Arc<NodeShared>, msg: NotifyMsg) {
    maybe_latency(&shared).await;
    let addrs = shared.peers_addrs.lock().await.clone();
    if addrs.is_empty() {
        warn!("no peer NodeAddrs known yet; using HTTP fallback");
        let body = serde_json::json!({
            "hash": &msg.hash,
            "filename": &msg.filename,
            "content_type": &msg.content_type,
            "provider_node_id": &msg.provider_node_id,
        })
        .to_string();
        for url in &shared.peers_http {
            let _ = reqwest::Client::new()
                .post(format!("{}/receive", url))
                .header("Content-Type", "application/json")
                .body(body.clone())
                .send()
                .await;
        }
        return;
    }
    for (url, addr) in addrs {
        maybe_latency(&shared).await;
        if let Err(e) = send_notify(&shared.endpoint, addr, &msg).await {
            warn!(?e, %url, "p2p notify failed; attempting HTTP fallback");
            let body = serde_json::json!({
                "hash": &msg.hash,
                "filename": &msg.filename,
                "content_type": &msg.content_type,
                "provider_node_id": &msg.provider_node_id,
            })
            .to_string();
            let _ = reqwest::Client::new()
                .post(format!("{}/receive", url))
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await;
        }
    }
}

async fn peer_addr_refresher(shared: Arc<NodeShared>) {
    let client = reqwest::Client::new();
    loop {
        for url in &shared.peers_http {
            if let Ok(resp) = client.get(format!("{}/status", url)).send().await {
                if let Ok(StatusPeerResp { node_addr }) = resp.json::<StatusPeerResp>().await {
                    if let Some(na) = node_addr
                        .and_then(|s| s.parse::<PublicKey>().ok())
                        .map(NodeAddr::from)
                    {
                        shared.peers_addrs.lock().await.insert(url.clone(), na);
                    }
                }
            }
        }
        sleep(Duration::from_millis(1000)).await;
    }
}

async fn maybe_latency(shared: &NodeShared) {
    let min = shared.latency_min;
    let max = shared.latency_max.max(min);
    if max == 0 {
        return;
    }
    let dur = {
        let mut rng = thread_rng();
        rng.gen_range(min..=max)
    };
    sleep(Duration::from_millis(dur)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_bytes_with_total() {
        let dbg = "DownloadProgress { received: 50, total: 200 }";
        let out = progress_bytes(&dbg).unwrap();
        assert_eq!(out.0, 50);
        assert_eq!(out.1, Some(200));
    }

    #[test]
    fn test_progress_bytes_only_received() {
        let dbg = "Ev { received: 123 }";
        let out = progress_bytes(&dbg).unwrap();
        assert_eq!(out.0, 123);
        assert_eq!(out.1, None);
    }

    #[test]
    fn test_status_peer_resp_serde() {
        let v: StatusPeerResp = serde_json::from_str("{\"node_addr\":null}").unwrap();
        assert!(v.node_addr.is_none());
        let v2: StatusPeerResp = serde_json::from_str("{\"node_addr\":\"abc\"}").unwrap();
        assert_eq!(v2.node_addr, Some("abc".to_string()));
    }
}
