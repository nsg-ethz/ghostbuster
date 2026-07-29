{
  inputs = { nixpkgs.url = "github:nixos/nixpkgs/nixos-25.11"; };

  outputs = { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };

      rust-addr2line = pkgs.rustPlatform.buildRustPackage rec {
        pname = "addr2line";
        version = "0.26.0";

        src = pkgs.fetchCrate {
          inherit pname version;
          sha256 = "sha256-UNtsQKhSBM8hOp9p8r2xeaTASaGm1/H/JiW5TUB7FMA=";
        };
        cargoHash = "sha256-oqPf3iaebsBWYNAzcVId9rCGGVIGMusb26v3yBN0e5g=";
        buildFeatures = [ "bin" ];
        meta = with pkgs.lib; {
          description =
            "A cross-platform library and CLI for retrieving per-address debug information";
          homepage = "https://github.com/gimli-rs/addr2line";
          license = with licenses; [ asl20 mit ];
          mainProgram = "addr2line";
        };
      };

    in {
      devShells."${system}".default = pkgs.mkShell {
        # nativeBuildInputs contains build tools (stuff that runs on the host)
        nativeBuildInputs = with pkgs; [ pkg-config ];

        # buildInputs contains libraries (stuff your app links against)
        buildInputs = with pkgs; [ openssl ];

        # This ensures that the OpenSSL environment variables are set correctly
        shellHook = ''
          export PKG_CONFIG_PATH="${pkgs.openssl.dev}/lib/pkgconfig"
        '';

        packages = with pkgs; [
          cargo
          rust-analyzer
          rustfmt
          rustc
          bacon
          cargo-expand
          clippy
          cargo-flamegraph
          perf
          rust-addr2line
        ];
      };
    };
}
