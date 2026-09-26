use ekapkgs_nix::bloom::BloomFilter;
use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;
use ekapkgs_protocol::ekapkgs::v1::{
    B3Digest, ChunkNegotiateRequest, ChunkNegotiateResponse, Compression, NarChunk,
    NegotiateRequest, NegotiateResponse, StreamNarsRequest,
};

/// Threshold above which we send a bloom filter instead of a flat list.
/// At 1000 chunks × 34 bytes ≈ 34 KB (flat list) vs ~1.2 KB (bloom filter).
const BLOOM_THRESHOLD: usize = 1_000;

/// Send a negotiate request to the ekapkgs cache server.
///
/// If `target` is provided, the server prioritizes the target and its
/// transitive runtime dependencies in the download plan (critical path
/// prioritization).
pub async fn negotiate(
    server_url: &str,
    want: Vec<String>,
    have: Vec<String>,
) -> color_eyre::Result<NegotiateResponse> {
    negotiate_with_target(server_url, want, have, None).await
}

/// Send a negotiate request with a target hash for critical path prioritization.
pub async fn negotiate_with_target(
    server_url: &str,
    want: Vec<String>,
    have: Vec<String>,
    target: Option<&str>,
) -> color_eyre::Result<NegotiateResponse> {
    let mut client = CacheServiceClient::connect(server_url.to_owned())
        .await?
        .max_decoding_message_size(64 * 1024 * 1024)
        .max_encoding_message_size(64 * 1024 * 1024);

    let request = tonic::Request::new(NegotiateRequest {
        want,
        have,
        accept_compression: vec![Compression::Zstd as i32, Compression::Xz as i32],
        trust_roots: Vec::new(),
        supports_cas: true,
        target_hash: target.unwrap_or_default().to_owned(),
    });

    let response = client.negotiate(request).await?;
    Ok(response.into_inner())
}

/// Send a chunk-level negotiate request to the ekapkgs cache server.
///
/// When the number of local chunks exceeds `BLOOM_THRESHOLD`, sends a compact
/// bloom filter instead of the full list of digests.
#[allow(dead_code)]
pub async fn negotiate_chunks(
    server_url: &str,
    want: Vec<String>,
    have: Vec<String>,
    have_chunks: Vec<[u8; 32]>,
) -> color_eyre::Result<ChunkNegotiateResponse> {
    let mut client = CacheServiceClient::connect(server_url.to_owned())
        .await?
        .max_decoding_message_size(64 * 1024 * 1024)
        .max_encoding_message_size(64 * 1024 * 1024);

    let request = if have_chunks.len() > BLOOM_THRESHOLD {
        let mut bf = BloomFilter::new(have_chunks.len());
        for d in &have_chunks {
            bf.insert(d);
        }
        let num_hashes = bf.num_hashes();
        let bloom_bytes = bf.into_bytes();
        tracing::debug!(
            "Sending bloom filter ({} bytes, k={}) for {} chunks",
            bloom_bytes.len(),
            num_hashes,
            have_chunks.len(),
        );
        tonic::Request::new(ChunkNegotiateRequest {
            want,
            have,
            have_chunks: Vec::new(),
            have_chunks_bloom: bloom_bytes,
            bloom_num_hashes: num_hashes,
        })
    } else {
        tonic::Request::new(ChunkNegotiateRequest {
            want,
            have,
            have_chunks: have_chunks
                .into_iter()
                .map(|d| B3Digest { digest: d.to_vec() })
                .collect(),
            have_chunks_bloom: Vec::new(),
            bloom_num_hashes: 0,
        })
    };

    let response = client.negotiate_chunks(request).await?;
    Ok(response.into_inner())
}

/// Start a NAR streaming session with the ekapkgs cache server.
///
/// Returns a gRPC stream of `NarChunk` messages. The server sends NAR data
/// for each requested path in order, split into 64 KiB chunks.
pub async fn stream_nars(
    server_url: &str,
    path_hashes: Vec<String>,
) -> color_eyre::Result<tonic::Streaming<NarChunk>> {
    let mut client = CacheServiceClient::connect(server_url.to_owned())
        .await?
        .max_decoding_message_size(64 * 1024 * 1024)
        .max_encoding_message_size(64 * 1024 * 1024);

    let request = tonic::Request::new(StreamNarsRequest { path_hashes });
    let response = client.stream_nars(request).await?;
    Ok(response.into_inner())
}
