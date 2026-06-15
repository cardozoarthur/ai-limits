# ai-limits

`ai-limits` is a small Rust CLI that prints usage-limit status for locally authenticated Codex and Gemini CLI accounts without opening either interactive TUI.

It is designed for quick terminal checks, scripts, and status bars. The default output is human-readable; `--json` emits structured data for automation.

## Features

- Reports Codex account limits through the Codex app-server API.
- Reports Gemini Code Assist quotas through the local Gemini OAuth session.
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
- Codex CLI installed and authenticated.
- Gemini CLI installed and authenticated.

The tool reads local CLI authentication state. It does not perform an interactive login.

## Usage

Show all providers:

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
ai-limits --provider codex,gemini --model spark,total --json
```

Increase timeout:

```sh
ai-limits --timeout-ms 45000
```

## Output

The human output includes provider status, CLI version, account plan or tier when available, model limits, percent used, percent remaining, reset time, and milliseconds until reset.

JSON output includes the same data in a stable structure:

```json
{
  "generated_at": "2026-01-01T00:00:00.000Z",
  "codex": {
    "name": "Codex",
    "status": "ok",
    "limits": []
  },
  "gemini": {
    "name": "Gemini",
    "status": "ok",
    "limits": []
  }
}
```

When `--provider` filters out a provider, that provider is omitted from JSON.

## Configuration

The CLI normally discovers local Codex and Gemini installations automatically.

Optional overrides:

- `AI_LIMITS_CODEX_CMD`: path to the Codex executable or command script.
- `AI_LIMITS_GEMINI_CMD`: path to the Gemini executable or command script.
- `AI_LIMITS_GEMINI_CLI_DIR`: path to the local `@google/gemini-cli` package directory.
- `AI_LIMITS_GEMINI_CLIENT_ID`: Gemini OAuth client id override.
- `AI_LIMITS_GEMINI_CLIENT_SECRET`: Gemini OAuth client secret override.

## Privacy

`ai-limits` reads local credential files only to request current quota status from the corresponding provider services. It does not log or print access tokens, refresh tokens, OAuth secrets, or authorization headers.

Avoid committing generated JSON reports because they may contain account identifiers, usage counts, or quota details. This repository ignores common local report filenames by default.

## License

MIT. See [LICENSE](LICENSE).
