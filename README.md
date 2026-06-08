# `tzctl`

`tzctl` manages Tenzir pipelines on a remote node through the Tenzir Platform.
It treats a local directory of `.tql` files as the source of truth, compares
that desired state with the pipelines that already exist on a node, and then
shows or applies the changes needed to bring them back in sync.

## What you can do with `tzctl`

- Sign in to the Tenzir Platform.
- List workspaces, nodes, and pipelines.
- Create, update, stop, and delete individual pipelines.
- Plan and apply declarative changes from a local project.
- Export machine-readable output with `--output json`.

## How it works

A `tzctl` project is a directory of `.tql` files plus a `tenzir.toml`
configuration file.

- Each `.tql` file defines one pipeline.
- The file name, or optional frontmatter `name`, identifies the pipeline.
- `tzctl project plan` shows the difference between your project and the node.
- `tzctl project apply` creates, updates, deletes, or changes pipeline state to
  match your project.

## Installation

```sh
uvx tzctl
uv tool install tzctl
cargo install tzctl
nix run github:tenzir/tzctl -- --help
nix profile add github:tenzir/tzctl
```

## Quick start

1. Authenticate:

   ```sh
   tzctl auth login
   ```

2. Select the target workspace and inspect available nodes:

   ```sh
   tzctl workspace list
   tzctl workspace select <workspace>
   tzctl node list
   ```

3. Configure your project in `tenzir.toml`:

   ```toml
   [workspace]
   id = "t-abcd1234"

   [node]
   id = "n-w2tjezz3"

   [pipelines]
   glob = "pipelines/**/*.tql"

   [defaults]
   state = "running"
   ```

4. Review changes before applying them:

   ```sh
   tzctl project plan
   ```

5. Apply the project:

   ```sh
   tzctl project apply
   ```

## Common commands

```sh
tzctl auth login
tzctl auth logout

tzctl workspace list
tzctl workspace select <id|name|#>
tzctl node list

tzctl pipeline list
tzctl pipeline create path/to/pipeline.tql
tzctl pipeline set path/to/pipeline.tql
tzctl pipeline stop <name>
tzctl pipeline delete <name>

tzctl project plan
tzctl project apply
tzctl project apply --dry-run
tzctl project apply --prune
tzctl project destroy
```

Run `tzctl --help` for the full command reference.

## Declarative pipeline projects

`tzctl` is most useful when you manage pipelines declaratively.

In this model, your repository contains the desired pipeline definitions and
states. `tzctl` compares that local project with the current node state and
reconciles the difference.

This approach helps you:

- Review changes before applying them.
- Keep pipeline definitions in version control.
- Reapply the same state safely and idempotently.
- Avoid accidental drift between environments.

## Pipeline files

A pipeline file can contain optional YAML frontmatter in line comments:

```tql
// ---
// name: zeek-import
// description: Import Zeek logs
// state: running
// ---
from file "/var/log/zeek/conn.log"
read zeek-tsv
```

Supported frontmatter fields:

- `name`
- `description`
- `state` (`running`, `paused`, or `stopped`)
- `node`

If a file has no frontmatter, `tzctl` uses the file stem as the pipeline name.

## Configuration

`tzctl` reads configuration from `tenzir.toml` and resolves values in this
order:

**CLI flags → `TENZIR_PLATFORM_CLI_*` environment variables → `tenzir.toml` → built-in defaults**

Example:

```toml
[platform]
api_endpoint = "https://rest.tenzir.app/production-v1"

[platform.oidc]
client_id = "vzRh8grIVu1bwutvZbbpBDCOvSzN8AXh"
# client_secret_file = "/run/secrets/tenzir-client-secret"

[workspace]
id = "t-abcd1234"

[node]
id = "n-w2tjezz3"

[pipelines]
glob = "pipelines/**/*.tql"

[defaults]
state = "running"
```

## Authentication

`tzctl auth login` authenticates with the Tenzir Platform and caches the token
for later commands.

You can authenticate in these ways:

- Interactive device-code login.
- Non-interactive client-credentials login through `tenzir.toml`.
- A pre-supplied token from configuration or environment variables.

## Safety and output

- `tzctl project plan` is read-only.
- Destructive actions prompt for confirmation unless you pass `--yes`.
- `--prune` deletes pipelines that exist on the node but not in your project.
- `--output json` prints structured output for automation and CI.

## Global options

| Option | Description |
| --- | --- |
| `--dir <DIR>` | Project directory to use. |
| `--config <FILE>` | Path to `tenzir.toml`. |
| `--workspace <ID>` | Workspace ID to target. |
| `--node <ID>` | Node ID to target. |
| `--yes` | Skip confirmation prompts. |
| `--output text\|json` | Output format. |
| `-v`, `--verbose` | Increase logging verbosity. |
| `--api-endpoint <URL>` | Override the platform API endpoint. |
