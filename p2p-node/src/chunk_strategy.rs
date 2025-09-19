use std::cmp::min;

use iroh_blobs::{
    protocol::{ChunkRanges, ChunkRangesExt, GetRequest},
    Hash,
};
use rand::{seq::SliceRandom, thread_rng, Rng};

/// Build a randomized list of `GetRequest`s covering the blob in fixed-size chunks.
///
/// This helper is experimental and not wired into the production download flow.
/// It can be used to explore alternative striping strategies where the chunk
/// order is shuffled before issuing fetches to peers.
#[allow(dead_code)]
pub fn randomized_get_requests(hash: Hash, total_chunks: u64, stripe_span: u64) -> Vec<GetRequest> {
    let mut rng = thread_rng();
    randomized_get_requests_with_rng(hash, total_chunks, stripe_span, &mut rng)
}

/// Same as [`randomized_get_requests`] but accepts an explicit RNG for testing.
pub fn randomized_get_requests_with_rng<R: Rng + ?Sized>(
    hash: Hash,
    total_chunks: u64,
    stripe_span: u64,
    rng: &mut R,
) -> Vec<GetRequest> {
    if total_chunks == 0 {
        return Vec::new();
    }
    let span = stripe_span.max(1);
    let mut offsets: Vec<u64> = (0..total_chunks).step_by(span as usize).collect();
    offsets.shuffle(rng);

    offsets
        .into_iter()
        .map(|start| {
            let end = min(total_chunks, start.saturating_add(span));
            let ranges = ChunkRanges::chunks(start..end);
            GetRequest::blob_ranges(hash.clone(), ranges)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rand::{rngs::StdRng, SeedableRng};

    use super::*;

    #[test]
    fn randomized_requests_cover_all_offsets() {
        let hash = Hash::from_bytes([1; 32]);
        let mut rng = StdRng::seed_from_u64(42);
        let requests = randomized_get_requests_with_rng(hash, 64, 8, &mut rng);
        assert_eq!(requests.len(), 8);
        assert!(requests.iter().all(|req| !req.ranges.is_empty()));
        let unique = requests
            .iter()
            .map(|req| format!("{:?}", req.ranges))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), requests.len());
    }
}
