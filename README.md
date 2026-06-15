# ai-limits

`ai-limits` is a small Rust CLI that prints usage-limit status for locally authenticated AI CLIs without opening their interactive TUIs.

It is designed for quick terminal checks, scripts, and status bars. The default output is human-readable; `--json` emits structured data for automation.

## Features

- Discovers supported local AI CLIs automatically.
- Reports Codex account limits through the Codex app-server API.
- Reports Gemini Code Assist quotas through the local Gemini OAuth session.
- Detects Claude CLI installations and exposes a placeholder provider for future quota support.
- Shows reset timestamps and milliseconds until reset.
- Separates Codex aggregate limits from model-specific limits such as Spark.
- Supports provider and model filters.
- Supports JSON output.
- Avoids printing access tokens, refresh tokens, or OAuth secrets.

## Installation

Build from source:

```powershell
cargo build --release
```

Then place the binary somewhere on your `PATH`:

```powershell
Copy-Item .\target\release\ai-limits.exe $env:USERPROFILE\bin\ai-limits.exe
```

On Unix-like systems:

```sh
cargo build --release
cp ./target/release/ai-limits ~/.local/bin/ai-limits
```

## Requirements

- Rust toolchain for building.
- At least one supported AI CLI installed.
- Provider-specific authentication for quota collection.

The tool reads local CLI authentication state. It does not perform an interactive login. If a supported CLI is not installed, it is omitted from the report, even when selected with `--provider`. Unknown provider names still return a validation error.

Currently supported providers:

| Provider | Auto-discovery | Limit collection |
| --- | --- | --- |
| Codex | Yes | Yes |
| Gemini | Yes | Yes |
| Claude | Yes | Placeholder only |

## Usage

Show all discovered providers:

```sh
ai-limits
```

Emit JSON:

```sh
ai-limits --json
```

Filter by provider:

```sh
ai-limits --provider codex
ai-limits --provider gemini
ai-limits --provider claude
```

Filter by model:

```sh
ai-limits --model spark
ai-limits --model total
ai-limits --model gemini-2.5-pro
```

Combine filters:

```sh
ai-limits --provider codex --model spark
ai-limits --provider codex,gemini,claude --model spark,total --json
```

Increase timeout:

```sh
ai-limits --timeout-ms 45000
```

## Output

The human output is in English and includes provider status, CLI version, account plan or tier when available, model limits, percent used, percent remaining, reset time, and milliseconds until reset.

JSON output includes the same data in a provider list. The list contains only providers that were found locally:

```json
{
  "generated_at": "2026-01-01T00:00:00.000Z",
  "providers": [
    {
      "key": "codex",
      "name": "Codex",
      "status": "ok",
      "limits": []
    }
  ]
}
```

When no supported CLI is found, the JSON report contains an empty `providers` array and exits successfully.

## Configuration

The CLI normally discovers local provider installations automatically. Discovery checks explicit overrides, known local package-manager locations, and PATH fallbacks.

Optional overrides:

- `AI_LIMITS_CODEX_CMD`: path to the Codex executable or command script.
- `AI_LIMITS_GEMINI_CMD`: path to the Gemini executable or command script.
- `AI_LIMITS_CLAUDE_CMD`: path to the Claude executable or command script.
- `AI_LIMITS_GEMINI_CLI_DIR`: path to the local `@google/gemini-cli` package directory.
- `AI_LIMITS_GEMINI_CLIENT_ID`: Gemini OAuth client id override.
- `AI_LIMITS_GEMINI_CLIENT_SECRET`: Gemini OAuth client secret override.

## Adding Providers

Providers are intentionally small. To add a provider:

1. Add the provider key to `ProviderKey`.
2. Add its default command and optional environment override.
3. Add any known local discovery paths in `cli_command_candidates_for_home`.
4. Implement a `collect_<provider>() -> Result<ProviderStatus>` function.
5. Add the provider to `collect_provider_by_key`.
6. Add tests for discovery, filtering, and output.

A provider can start as detection-only by returning `ProviderStatus` with no limits and a `source` message explaining that quota collection is not implemented yet. Claude currently uses this pattern.

## Privacy

`ai-limits` reads local credential files only to request current quota status from the corresponding provider services. It does not log or print access tokens, refresh tokens, OAuth secrets, or authorization headers.

Avoid committing generated JSON reports because they may contain account identifiers, usage counts, or quota details. This repository ignores common local report filenames by default.

## License

MIT. See [LICENSE](LICENSE).
