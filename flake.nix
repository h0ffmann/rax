{
  description = "RAX, the Rustic Agentic Runtime: the README drawing, 代理 in a small cosmos, in pure Nix";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      drawing = import ./drawings { inherit lib; };
      forAll = lib.genAttrs [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
    in
    {
      packages = forAll (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in {
          # The README drawing (drawings/): cosmos.svg, cosmos.desmos.json and cosmos-check.svg.
          cosmos-art = pkgs.linkFarm "cosmos-art" [
            { name = "cosmos.svg"; path = pkgs.writeText "cosmos.svg" drawing.svg; }
            { name = "cosmos.desmos.json"; path = pkgs.writeText "cosmos.desmos.json" drawing.desmos; }
            { name = "cosmos-check.svg"; path = pkgs.writeText "cosmos-check.svg" drawing.checkSvg; }
          ];
        });

      checks = forAll (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in {
          # The drawing's tests pass and the committed files are its output (`just drawing`).
          drawing = pkgs.runCommand "cosmos-art-check" { } ''
            ${lib.concatMapStrings (f: "echo 'failed: ${f.name}' >&2\n") drawing.failures}
            ${lib.optionalString (drawing.failures != [ ]) "exit 1"}
            art=${self.packages.${system}.cosmos-art}
            diff -u ${./docs/drawings/cosmos.svg} $art/cosmos.svg || { echo 'run `just drawing`' >&2; exit 1; }
            diff -u ${./docs/drawings/cosmos.desmos.json} $art/cosmos.desmos.json || { echo 'run `just drawing`' >&2; exit 1; }
            touch $out
          '';
        });
    };
}
