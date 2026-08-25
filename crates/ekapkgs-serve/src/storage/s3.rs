//! S3-compatible object storage backend.
//!
//! Stores narinfos at `{prefix}{hash}.narinfo` and NARs at `{prefix}nar/{file}`
//! in an S3 bucket. Supports AWS S3, Cloudflare R2, MinIO, and any
//! S3-compatible endpoint.

use aws_sdk_s3::Client;
use aws_sdk_s3::primitives::ByteStream;

use super::{NarInfo, StorageBackend};

/// S3 storage backend configuration.
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub prefix: String,
}

/// Storage backend backed by an S3-compatible object store.
pub struct S3Backend {
    client: Client,
    bucket: String,
    prefix: String,
    rt: tokio::runtime::Handle,
}

impl S3Backend {
    /// Create a new S3 backend from the given configuration.
    ///
    /// Uses the standard AWS credential chain (environment variables,
    /// instance profile, config files, etc.).
    pub async fn new(config: S3Config) -> color_eyre::Result<Self> {
        let mut aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(config.region));

        if let Some(endpoint) = &config.endpoint {
            aws_config = aws_config.endpoint_url(endpoint);
        }

        let aws_config = aws_config.load().await;

        let mut s3_config = aws_sdk_s3::config::Builder::from(&aws_config);

        // Force path-style for non-AWS endpoints (R2, MinIO, etc.)
        if config.endpoint.is_some() {
            s3_config = s3_config.force_path_style(true);
        }

        let client = Client::from_conf(s3_config.build());

        Ok(Self {
            client,
            bucket: config.bucket,
            prefix: config.prefix,
            rt: tokio::runtime::Handle::current(),
        })
    }

    fn key(&self, path: &str) -> String {
        format!("{}{path}", self.prefix)
    }

    async fn get_object(&self, key: &str) -> color_eyre::Result<Option<Vec<u8>>> {
        let result = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;

        match result {
            Ok(output) => {
                let data = output.body.collect().await?.to_vec();
                Ok(Some(data))
            },
            Err(err) => {
                if is_not_found(&err) {
                    Ok(None)
                } else {
                    Err(err.into())
                }
            },
        }
    }

    async fn put_object(&self, key: &str, data: Vec<u8>) -> color_eyre::Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data))
            .send()
            .await?;
        Ok(())
    }

    async fn head_object(&self, key: &str) -> color_eyre::Result<bool> {
        let result = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;

        match result {
            Ok(_) => Ok(true),
            Err(err) => {
                if is_not_found(&err) {
                    Ok(false)
                } else {
                    Err(err.into())
                }
            },
        }
    }
}

impl StorageBackend for S3Backend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn has_narinfo(&self, hash: &str) -> color_eyre::Result<bool> {
        let key = self.key(&format!("{hash}.narinfo"));
        self.rt.block_on(self.head_object(&key))
    }

    fn get_narinfo(&self, hash: &str) -> color_eyre::Result<Option<NarInfo>> {
        let Some(text) = self.get_narinfo_text(hash)? else {
            return Ok(None);
        };
        Ok(NarInfo::parse(&text))
    }

    fn get_narinfo_text(&self, hash: &str) -> color_eyre::Result<Option<String>> {
        let key = self.key(&format!("{hash}.narinfo"));
        let data = self.rt.block_on(self.get_object(&key))?;
        Ok(data.map(|d| String::from_utf8_lossy(&d).into_owned()))
    }

    fn get_nar(&self, file_path: &str) -> color_eyre::Result<Option<Vec<u8>>> {
        let key = self.key(file_path);
        self.rt.block_on(self.get_object(&key))
    }

    fn put_narinfo(&self, hash: &str, content: &str) -> color_eyre::Result<bool> {
        let key = self.key(&format!("{hash}.narinfo"));
        self.rt
            .block_on(self.put_object(&key, content.as_bytes().to_vec()))?;
        Ok(true)
    }

    fn put_nar(&self, file_path: &str, data: &[u8]) -> color_eyre::Result<bool> {
        let key = self.key(file_path);
        self.rt.block_on(self.put_object(&key, data.to_vec()))?;
        Ok(true)
    }
}

/// Check if an S3 error is a 404 Not Found.
fn is_not_found<E: std::fmt::Debug>(err: &aws_sdk_s3::error::SdkError<E>) -> bool {
    matches!(err, aws_sdk_s3::error::SdkError::ServiceError(e) if e.raw().status().as_u16() == 404)
}
