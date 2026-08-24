pub mod ekapkgs {
    pub mod v1 {
        tonic::include_proto!("ekapkgs.v1");
    }
}

pub use ekapkgs::v1::*;
