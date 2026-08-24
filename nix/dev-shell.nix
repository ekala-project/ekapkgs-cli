{
  stdenv,
  fenix,
  pkg-config,
  protobuf,
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
}
