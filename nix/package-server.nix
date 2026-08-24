{
  lib,
  rustPlatform,
  protobuf,
  nix,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "ekapkgs-serve";
  version =
    let
      cargo_toml = builtins.readFile ../crates/ekapkgs-serve/Cargo.toml;
      cargo_info = builtins.fromTOML cargo_toml;
    in
    cargo_info.package.version;

  cargoLock.lockFile = ../Cargo.lock;
  src = ../.;

  cargoBuildFlags = [
    "-p"
    "ekapkgs-serve"
  ];
  cargoTestFlags = [
    "-p"
    "ekapkgs-serve"
  ];

  nativeBuildInputs = [
    protobuf
    makeWrapper
  ];

  doCheck = false;

  postFixup = ''
    wrapProgram $out/bin/ekapkgs-serve \
      --prefix PATH : ${lib.makeBinPath [ nix ]}
  '';

  meta = with lib; {
    description = "ekapkgs binary cache server with negotiation protocol";
    mainProgram = "ekapkgs-serve";
  };
}
