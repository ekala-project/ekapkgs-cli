{
  lib,
  rustPlatform,
  protobuf,
  nix,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "ekapkgs";
  version =
    let
      cargo_toml = builtins.readFile ../crates/ekapkgs/Cargo.toml;
      cargo_info = builtins.fromTOML cargo_toml;
    in
    cargo_info.package.version;

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
    makeWrapper
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
