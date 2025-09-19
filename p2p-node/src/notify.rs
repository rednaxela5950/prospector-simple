#[cfg(all(not(test), feature = "p2p_notify"))]
use crate::NodeShared;
use iroh::Endpoint;
#[cfg(all(not(test), feature = "p2p_notify"))]
use iroh_base::{NodeAddr, PublicKey};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::timeout;
#[cfg(all(not(test), feature = "p2p_notify"))]
use {
    iroh::endpoint::Connection,
    iroh::protocol::{AcceptError, ProtocolHandler},
    std::sync::Arc,
};

pub const NOTIFY_ALPN: &[u8] = b"/iroh-demo/image-notify/1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyMsg {
    pub hash: String,
    pub filename: String,
    pub content_type: String,
    pub provider_node_id: Option<String>,
}

/// Accept incoming notify messages (JSON) and kick off a download (only when p2p_notify feature is enabled)
#[cfg(all(not(test), feature = "p2p_notify"))]
#[derive(Debug)]
pub struct NotifyHandler {
    pub shared: Arc<NodeShared>,
}

#[cfg(all(not(test), feature = "p2p_notify"))]
impl ProtocolHandler for NotifyHandler {
    fn accept(
        &self,
        conn: Connection,
    ) -> impl std::future::Future<Output = Result<(), AcceptError>> + Send {
        let shared = self.shared.clone();
        async move {
            // In iroh 0.91, accept_bi yields (SendStream, RecvStream)
            let (mut send, mut recv) = conn.accept_bi().await?;
            // Limit JSON message size to 256 KiB
            let body = recv
                .read_to_end(256 * 1024)
                .await
                .map_err(AcceptError::from_err)?;
            let msg: NotifyMsg = serde_json::from_slice(&body).map_err(AcceptError::from_err)?;
            let hash: iroh_blobs::Hash = msg.hash.parse().map_err(AcceptError::from_err)?;
            let fallback: Option<NodeAddr> = match msg.provider_node_id.as_deref() {
                Some(pk) => pk.parse::<PublicKey>().ok().map(NodeAddr::from),
                None => None,
            };
            if let Err(e) = shared
                .receive_by_discovery(hash, msg.filename, msg.content_type, fallback)
                .await
            {
                tracing::error!(?e, "notify receive_by_discovery failed");
                // We still respond on the stream, but don't fail the accept
            }
            let _ = send.write_all(b"ok").await;
            let _ = send.finish();
            Ok(())
        }
    }
}

/// Helper to send a notify message to a peer
pub async fn send_notify(
    endpoint: &Endpoint,
    node_addr: iroh_base::NodeAddr,
    msg: &NotifyMsg,
) -> anyhow::Result<()> {
    let conn = endpoint.connect(node_addr, NOTIFY_ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    let body = serde_json::to_vec(msg)?;
    send.write_all(&body).await?;
    send.finish()?;
    // Wait briefly for an ACK from the peer to reduce benign close warnings
    let _ = timeout(Duration::from_millis(1500), recv.read_to_end(64)).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notify_msg_roundtrip() {
        let msg = NotifyMsg {
            hash: "abc123".into(),
            filename: "f.png".into(),
            content_type: "image/png".into(),
            provider_node_id: Some("prov".into()),
        };
        let s = serde_json::to_string(&msg).unwrap();
        let back: NotifyMsg = serde_json::from_str(&s).unwrap();
        assert_eq!(back.hash, "abc123");
        assert_eq!(back.filename, "f.png");
        assert_eq!(back.content_type, "image/png");
        assert_eq!(back.provider_node_id.as_deref(), Some("prov"));
    }

    #[test]
    fn test_notify_alpn_value() {
        assert_eq!(NOTIFY_ALPN, b"/iroh-demo/image-notify/1");
    }
}
