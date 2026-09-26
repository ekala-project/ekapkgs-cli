{
  stdenv,
  fenix,
  pkg-config,
  protobuf,
  fuse3,
}:

stdenv.mkDerivation {
  name = "dev";

  nativeBuildInputs = [
    protobuf
    pkg-config
    (fenix.default.withComponents [
      "cargo"
      "clippy"
      "rust-std"
      "rustc"
      "rustfmt-preview"
    ])
  ];

  buildInputs = [
    fuse3
  ];
}
