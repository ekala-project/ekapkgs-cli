{
  lib,
  rustPlatform,
  protobuf,
  pkg-config,
  fuse3,
  nix,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "ekapkgs";
  version =
    let
      cargo_toml = builtins.readFile ../Cargo.toml;
      cargo_info = builtins.fromTOML cargo_toml;
    in
    cargo_info.workspace.package.version;

  cargoLock.lockFile = ../Cargo.lock;
  src = ../.;

  cargoBuildFlags = [
    "-p"
    "ekapkgs"
  ];
  cargoTestFlags = [
    "-p"
    "ekapkgs"
  ];

  nativeBuildInputs = [
    protobuf
    pkg-config
    makeWrapper
  ];

  buildInputs = [
    fuse3
  ];

  doCheck = false;

  postFixup = ''
    wrapProgram $out/bin/ekapkgs \
      --prefix PATH : ${lib.makeBinPath [ nix ]}
  '';

  meta = with lib; {
    description = "Nix CLI wrapper with negotiated binary cache protocol";
    mainProgram = "ekapkgs";
  };
}
