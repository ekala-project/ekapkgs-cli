use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;
use ekapkgs_protocol::ekapkgs::v1::{Compression, NegotiateRequest, NegotiateResponse};

/// Send a negotiate request to the ekapkgs cache server.
pub async fn negotiate(
    server_url: &str,
    want: Vec<String>,
    have: Vec<String>,
) -> color_eyre::Result<NegotiateResponse> {
    let mut client = CacheServiceClient::connect(server_url.to_string()).await?;

    let request = tonic::Request::new(NegotiateRequest {
        want,
        have,
        accept_compression: vec![Compression::Zstd as i32, Compression::Xz as i32],
        trust_roots: Vec::new(),
    });

    let response = client.negotiate(request).await?;
    Ok(response.into_inner())
}
