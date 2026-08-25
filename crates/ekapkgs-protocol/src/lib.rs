pub mod ekapkgs {
    // The generated tonic server trait methods return Result<_, tonic::Status>
    // where Status is > 128 bytes. This is inherent to tonic and cannot be changed.
    #[allow(clippy::result_large_err)]
    pub mod v1 {
        tonic::include_proto!("ekapkgs.v1");
    }
}

pub mod signing;

pub use ekapkgs::v1::*;
