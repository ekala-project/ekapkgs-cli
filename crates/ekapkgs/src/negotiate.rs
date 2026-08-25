use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;
use ekapkgs_protocol::ekapkgs::v1::{
    B3Digest, ChunkNegotiateRequest, ChunkNegotiateResponse, Compression, NegotiateRequest,
    NegotiateResponse,
};

/// Send a negotiate request to the ekapkgs cache server.
pub async fn negotiate(
    server_url: &str,
    want: Vec<String>,
    have: Vec<String>,
) -> color_eyre::Result<NegotiateResponse> {
    let mut client = CacheServiceClient::connect(server_url.to_owned()).await?;

    let request = tonic::Request::new(NegotiateRequest {
        want,
        have,
        accept_compression: vec![Compression::Zstd as i32, Compression::Xz as i32],
        trust_roots: Vec::new(),
        supports_cas: true,
    });

    let response = client.negotiate(request).await?;
    Ok(response.into_inner())
}

/// Send a chunk-level negotiate request to the ekapkgs cache server.
pub async fn negotiate_chunks(
    server_url: &str,
    want: Vec<String>,
    have: Vec<String>,
    have_chunks: Vec<[u8; 32]>,
) -> color_eyre::Result<ChunkNegotiateResponse> {
    let mut client = CacheServiceClient::connect(server_url.to_owned()).await?;

    let request = tonic::Request::new(ChunkNegotiateRequest {
        want,
        have,
        have_chunks: have_chunks
            .into_iter()
            .map(|d| B3Digest { digest: d.to_vec() })
            .collect(),
    });

    let response = client.negotiate_chunks(request).await?;
    Ok(response.into_inner())
}
