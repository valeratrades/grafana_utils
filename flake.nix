{
  inputs = {
    v_flakes.url = "github:valeratrades/v_flakes?ref=v1.6";
  };

  outputs = { self, v_flakes }:
    let
      inherit (v_flakes) flake-utils;
    in
    {
      # `serve <db> <dashboards dir> <data dir>`: Grafana over one SQLite file, with
      # the dashboards in the directory provisioned and re-read as they change.
      lib.grafana = pkgs: { name, plugins ? { }, ini ? { } }:
        let
          yaml = pkgs.formats.yaml { };
          # linkFarm, not symlinkJoin: Grafana walks one directory per plugin, and
          # merging their contents into a single flat tree makes discovery fail.
          pluginDir = pkgs.linkFarm "${name}-grafana-plugins" ({ frser-sqlite-datasource = pkgs.grafanaPlugins.frser-sqlite-datasource; } // plugins);
          provisioning = pkgs.runCommand "${name}-grafana-provisioning" { } ''
            mkdir -p $out/datasources $out/dashboards $out/plugins $out/alerting
            cp ${yaml.generate "datasource.yaml" {
              apiVersion = 1;
              datasources = [{
                inherit name;
                uid = name;
                type = "frser-sqlite-datasource";
                access = "proxy";
                isDefault = true;
                jsonData.path = "$GRAFANA_UTILS_DB";
              }];
            }} $out/datasources/${name}.yaml
            cp ${yaml.generate "dashboards.yaml" {
              apiVersion = 1;
              providers = [{
                inherit name;
                type = "file";
                # Rewritten on every export; the poll is what picks an edit up.
                allowUiUpdates = false;
                updateIntervalSeconds = 10;
                options = { path = "$GRAFANA_UTILS_DASH"; foldersFromFilesStructure = false; };
              }];
            }} $out/dashboards/${name}.yaml
          '';
          # One user, one machine, read-only data: a login screen would only ever be
          # something to click past. `Main Org.` is matched literally, period included.
          config = (pkgs.formats.ini { }).generate "grafana.ini" (pkgs.lib.recursiveUpdate {
            server.http_addr = "127.0.0.1";
            auth.disable_login_form = true;
            "auth.anonymous" = { enabled = true; org_name = "Main Org."; };
            # Grafana 13 pins anonymous access to Viewer; this is what still lets a
            # panel query be tweaked in the UI before it is written back to JSON.
            users.viewers_can_edit = true;
            # Grafana fetches a preinstall list on first boot; the plugin directory is
            # a store path, so that would only ever fail loudly.
            plugins.preinstall_disabled = true;
            analytics = { reporting_enabled = false; check_for_updates = false; check_for_plugin_updates = false; };
            news.news_feed_enabled = false;
          } ini);
        in
        pkgs.writeShellApplication {
          name = "${name}-grafana";
          runtimeInputs = [ pkgs.grafana ];
          text = ''
            if [ $# -ne 3 ]; then
              echo "usage: ${name}-grafana <db> <dashboards dir> <data dir>" >&2
              exit 1
            fi
            GRAFANA_UTILS_DB=$1 GRAFANA_UTILS_DASH=$2 \
            GF_PATHS_DATA=$3 \
            GF_PATHS_PLUGINS=${pluginDir} \
            GF_PATHS_PROVISIONING=${provisioning} \
              exec grafana server --homepath ${pkgs.grafana}/share/grafana --config ${config}
          '';
        };
    }
    // flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import v_flakes.default_nixpkgs { inherit system; };
        rust = v_flakes.rs.default_nightly system;
        manifest = (pkgs.lib.importTOML ./Cargo.toml).package;
        rustPlatform = pkgs.makeRustPlatform { rustc = rust; cargo = rust; };
      in
      {
        packages.default = rustPlatform.buildRustPackage {
          pname = manifest.name;
          version = manifest.version;
          RUSTC_WRAPPER = ""; # sccache, if the user config sets it, is absent in the sandbox
          cargoLock.lockFile = ./Cargo.lock;
          src = pkgs.lib.cleanSource ./.;
        };

        devShells.default = pkgs.mkShell { packages = [ rust ]; };
      });
}
