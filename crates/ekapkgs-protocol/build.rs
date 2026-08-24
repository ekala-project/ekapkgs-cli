fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "../../proto";

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                format!("{proto_root}/ekapkgs/v1/negotiate.proto"),
                format!("{proto_root}/ekapkgs/v1/manifest.proto"),
                format!("{proto_root}/ekapkgs/v1/signing.proto"),
            ],
            &[proto_root],
        )?;

    Ok(())
}
