{
  stdenv,
  fenix,
  pkg-config,
  protobuf,
  fuse3,
  iets,
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
    iets
  ];

  buildInputs = [
    fuse3
  ];
}
