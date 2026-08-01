{
  description = "Agent-first development environment for dpm";

  # 26.05 remains the current line that supports all four release targets,
  # including x86_64-darwin.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { nixpkgs, ... }:
    let
      supportedSystems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
      mkPkgs = system: import nixpkgs { inherit system; };
      mkAgentCheck =
        pkgs:
        pkgs.writeShellApplication {
          name = "agent-check";
          runtimeInputs = with pkgs; [
            bash
            cargo
            clippy
            git
            rustc
          ];
          text = ''
            exec bash ${./.nix/agent-check.sh} "$@"
          '';
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          agentCheck = mkAgentCheck pkgs;
        in
        {
          agent-check = agentCheck;
          default = agentCheck;
        }
      );

      apps = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          agentCheck = mkAgentCheck pkgs;
        in
        {
          agent-check = {
            type = "app";
            program = "${agentCheck}/bin/agent-check";
          };
          default = {
            type = "app";
            program = "${agentCheck}/bin/agent-check";
          };
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
        in
        {
          agent-check = mkAgentCheck pkgs;
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              (mkAgentCheck pkgs)
              cargo
              clippy
              curl
              docker-client
              git
              jq
              pkg-config
              postgresql_16
              rustc
              rustfmt
            ];
          };
        }
      );

      formatter = forAllSystems (system: (mkPkgs system).nixfmt-tree);
    };
}
