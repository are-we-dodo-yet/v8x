{
  description = "v82jsc: v8 C-ABI compatibility layer over JavaScriptCore / QuickJS";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];

      perSystem = { pkgs, system, ... }:
        let
          pkgs' = import inputs.nixpkgs {
            inherit system;
            overlays = [ inputs.rust-overlay.overlays.default ];
          };
          rustToolchain = pkgs'.rust-bin.stable.latest.default.override {
            extensions = [ "rust-src" "rust-analyzer" ];
          };
          llvm = pkgs'.llvmPackages_20;
        in
        {
          devShells.default = pkgs'.mkShell {
            packages = with pkgs'; [
              rustToolchain
              cargo-nextest
              llvm.clang
              llvm.lld
              llvm.libllvm
              pkg-config
              cmake
              ninja
              nodejs_22
              jq
              ruby
              perl
              python3
              git
              typst
            ];

            buildInputs = with pkgs'; [
              icu
            ] ++ lib.optionals stdenv.hostPlatform.isLinux [
              glib
            ];

            LIBCLANG_PATH = pkgs'.lib.makeLibraryPath [ llvm.clang-unwrapped.lib ];
            SSL_CERT_FILE = "${pkgs'.cacert}/etc/ssl/certs/ca-bundle.crt";

            shellHook = ''
              unset AS
            '';
          };
        };
    };
}
