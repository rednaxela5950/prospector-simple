# Emerald Image Mesh – Code-Level Architecture & Design Notes

This document explains the code structure and design choices for both the Rust P2P node (`p2p-node/`) and the React UI (`ui/`). It is written for junior developers and focuses on the “why,” not just the “what”.

- Rust service: `p2p-node/`
- UI: `ui/`

---

## High-Level Flow

1. A user uploads an image to one node (the provider) via HTTP `/upload`.
2. The provider adds the blob to its local iroh-blobs store and exposes a `ticket` (and `hash`).
3. The provider notifies peers with the `hash` (via P2P notify and/or HTTP fallback).
4. Peers discover the provider using the `hash` and download the blob over the iroh-blobs protocol.
5. During download, peers stream live progress into their HTTP-visible `NodeState`.
6. Only after the full blob is received and exported to `current.img` do peers flip `has_image = true`.

Why this order? It prevents the UI from thinking an image is “available” on peers before it is fully persisted to disk and export is complete.

---

## Rust Node (`p2p-node/src/main.rs`)

### Key Types

- `NodeShared`: central state shared across handlers.
  - Fields:
    - `endpoint: iroh::Endpoint` – network endpoint for iroh protocols.
    - `blobs: iroh_blobs::BlobsProtocol` – P2P blobs protocol instance.
    - `store: Arc<FsStore>` – filesystem-backed blob store.
    - `state: Arc<Mutex<NodeState>>` – current HTTP-visible node status (thread-safe via `tokio::sync::Mutex`).
    - `data_dir: PathBuf` – where we write `current.img` for HTTP serving.
    - `peers_http: Vec<String>` – peer base URLs for fallback or discovery.
    - `peers_addrs: Arc<Mutex<HashMap<String, NodeAddr>>>` – resolved iroh `NodeAddr` map for P2P notify.
    - Latency knobs: `latency_min`, `latency_max`, `stream_sleep_ms` for demos/tests.

- `NodeState` (reported at `/status`):
  - `has_image: bool` – flips to `true` only after full download + export complete.
  - `current_filename`, `content_type`, `current_hash` – metadata for the active content.
  - `bytes_total: Option<u64>` – total size if known; may be `None` during transfer.
  - `bytes_received: u64` – running byte count during download.
  - `progress: f32` – percentage when `bytes_total` is known; otherwise derived at completion.
  - `stripe_providers: HashMap<String, Vec<String>>` – maps provider node IDs to the stripe labels they delivered.

Why `Mutex<NodeState>`? Multiple async tasks (HTTP handlers, timers, download stream) update/read the state. `Mutex` provides safe exclusive access.

### HTTP Endpoints

- `GET /status` → returns `NodeState` as JSON.
- `GET /image` → returns entire current image as a single response.
- `GET /image_stream` → streams the image with tiny sleeps between chunks.
  - Why? Encourages visible progressive rendering in the browser for demos.
  - Uses `ReaderStream` and optional `STREAM_SLEEP_MS` delays.
- `POST /upload` → accepts multipart `file`, converts it into a blob, writes `current.img`, updates `NodeState`, and notifies peers.
  - Sets `bytes_total = total`, `bytes_received = total`, `progress = 100` on the provider (upload is a one-shot write, not a P2P download).
- `POST /receive` → accepts either a full ticket or just a `hash` and initiates peer-side download.

### Peer Discovery & Notify

- `peer_addr_refresher(shared)`
  - Periodically polls peers’ `/status` to resolve their iroh `NodeAddr` from `node_addr` and caches in `peers_addrs`.
  - Why? The iroh P2P notify requires `NodeAddr`. If unknown, we fallback to HTTP.

- `notify_all_peers(shared, msg)` 
  - Defined in `p2p-node/src/main.rs`.
  - Attempts P2P notify using known `NodeAddr`s with the `send_notify` helper function defined in `p2p-node/src/notify.rs`.
  - On failure or if no addresses are known yet, falls back to HTTP `POST /receive`.
  - Why dual-path? Ensures reliability in early boot/unstable discovery phases.

### Download With Streaming Progress

Two entry points perform downloads and report progress the same way:

- `NodeShared::receive_by_discovery(hash, filename, content_type, fallback)`
- `NodeShared::receive_with_progress(ticket, filename, content_type)`

Both now use iroh-blobs’ progress stream:

```rust
let downloader = self.store.downloader(&self.endpoint);
let dl = downloader.download(hash, Some(provider_node_id));
let mut stream = dl.stream().await?;
while let Some(item) = stream.next().await {
    match item {
        DownloadProgessItem::Progress(recvd) => {
            let mut s = self.state.lock().await;
            s.bytes_received = recvd;
            if let Some(t) = s.bytes_total {
                if t > 0 { s.progress = (recvd as f32 / t as f32) * 100.0; }
            }
        }
        DownloadProgessItem::TryProvider { .. } => {}
        DownloadProgessItem::ProviderFailed { .. } => {}
        DownloadProgessItem::PartComplete { .. } => {}
        DownloadProgessItem::Error(e) => { /* treat as failure */ }
        DownloadProgessItem::DownloadError => { /* treat as failure */ }
    }
}
```

Design choices:
- We update `bytes_received` on every `Progress(recvd)` event.
- `bytes_total` is often unknown during transfer with the current API; we keep it `None` until we know it or set it equal to `bytes_received` at completion.
- `has_image` only flips to `true` after we export the blob to `current.img`:
  - Export: `self.store.blobs().export(hash, &out_path).await`.
  - Then set `has_image = true` and `progress = 100.0`.
  - Why? Guarantees the HTTP `/image` and `/image_stream` endpoints immediately serve the completed file.

Error handling:
- We handle all progress variants (`TryProvider`, `ProviderFailed`, `PartComplete`, `Error`, `DownloadError`).
- Any error terminates the attempt; we may try other candidates (in `receive_by_discovery`).

Concurrency:
- Every state update acquires `self.state.lock().await` briefly, keeping the critical sections tiny.
- This is safe for the frequency of progress updates in demos; for very large blobs and high-frequency updates, consider rate-limiting UI state writes.

Latency simulation:
- `maybe_latency(shared)` injects artificial latency (env vars `LATENCY_MS_MIN/MAX`) to make progress visibly update.
- `image_stream` sleeps per chunk (`STREAM_SLEEP_MS`) to demonstrate progressive rendering.

### Why set `bytes_total = Some(bytes_received)` at completion?

When the total size is unknown throughout transfer, the UI can still show a sensible terminal state once the download ends. Setting `bytes_total` to `bytes_received` makes `progress = 100` match a consistent final size. If/when the API provides a reliable total upfront, we can set it as soon as it’s known and show accurate percentages during transfer.

---

## UI (`ui/src/`)

### `api.ts`
- Provides the list of nodes (`nodes`), derived from `VITE_NODES_JSON` in `ui/.env.local`.
- Fetch helpers:
  - `getStatus(n)` → calls node `/status`.
  - `getImageUrl(n)` → `/image`.
  - `getStreamUrl(n)` → `/image_stream`.
  - `uploadTo(n, file)` → `POST /upload` with multipart.
  - `fanoutHash(hash, filename, content_type, provider_node_id, except)` → HTTP fallback broadcast to `/receive`.

Why version the image URLs? The UI appends a query param `?v=...` with the current `hash` or `progress` to bust caches and ensure the browser fetches the newest image stream each update.

### `App.tsx`
- Controls the scenario: upload → fan-out → observe peers.
- Steps UI (`prepare`, `upload`, `notify`, `discovery`, `download`, `complete`) reflects the end-to-end process.
- Polls all nodes’ `/status` every 500ms and updates the steps heuristically:
  - `discovery` is done if any peer reports `current_hash === uploaded.hash`.
  - `download` is considered underway if any peer shows `progress > 0` for that hash and `has_image == false`.
  - `complete` is done when all other peers show `has_image == true` with the same `hash`.

Design notes:
- Provider selection defaults to the first node for simplicity.
- `ensureJpeg(file)` converts non-JPEGs to JPEG before upload to minimize unexpected format handling on the server.

### `Graph.tsx`
- Renders peers in a static ring with per-node progress rings.
- Disables d3 simulation (forces) for a calm, static layout.
- Manual drag support keeps positions stable even across library versions; positions persist in `localStorage` (`emerald_graph_positions`).
- Node visuals:
  - Gray outer ring.
  - Blue/green progress arc (green when `>= 100%`).
  - Center shows the node’s `image_stream` if `has_image` is true; cache-busted with `?v=current_hash`.

Why compute layout deterministically?
- We seed positions based on deterministic angles and the container size so that nodes are immediately visible and do not “fly around” when status updates arrive.

---

## Testing the Progress Flow

1. Start the cluster: see `docker-compose.yml`.
2. Upload a large image to `node-a` using the UI.
3. Observe peers’ `/status` (e.g., `http://localhost:4002/status`) – `bytes_received` increases; `has_image` stays `false`.
4. When download finishes and export completes, `has_image` flips to `true`, `progress` becomes `100`, and `image_stream` renders.

Tip: Increase `LATENCY_MS_MIN/MAX` and/or use a larger file to better observe progress updates.

---

## Future Improvements

- Capture and expose total size (`bytes_total`) earlier if/when iroh-blobs progress exposes a reliable total; update UI percentage continuously.
- Add rate-limited state updates to avoid excessive locking at very high event frequencies.
- Add structured error reporting to `/status` when a download fails (e.g., last error message per peer).
- Add small e2e tests for `/upload` → notify → `/receive` path using a temporary store directory.

---

## Quick Reference: Important Symbols

- File `p2p-node/src/main.rs`:
  - `NodeShared`
  - `NodeShared::receive_by_discovery()`
  - `NodeShared::receive_with_progress()`
  - `peer_addr_refresher()`
  - `notify_all_peers()` (defined here; P2P send helper `send_notify` is in `p2p-node/src/notify.rs`)
  - HTTP handlers: `status`, `image`, `image_stream`, `upload`, `receive_http`

- File `ui/src/api.ts`:
  - `nodes`, `getStatus`, `getImageUrl`, `getStreamUrl`, `uploadTo`, `fanoutHash`

- File `ui/src/App.tsx`:
  - Steps and polling logic; calls API helpers.

- File `ui/src/Graph.tsx`:
  - Static layout and progress rings with cached images.

---

## Rationale Recap

- Progress streaming is implemented using `iroh_blobs::api::downloader::DownloadProgessItem` to provide real-time telemetry.
- `has_image` is set to `true` only after export to `current.img` to guarantee the UI can immediately stream the image.
- The UI polls `/status` frequently and visualizes progress in a stable, non-animated graph for clarity.
