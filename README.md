# rusty-claude

A portable **Rust supervisor** for the official Claude CLI that adds:

- ✅ Smart retries on 429 / 5xx / “overloaded” errors (honors `Retry-After` if present)  
- ✅ Exponential backoff + jitter
- ✅ Stdin replay on retries (so piped requests aren’t lost)  
- ✅ Live tee of stdout and stderr (you see output as it happens)
- ✅ Interactive-by-default from a TTY (Windows & Linux)

This tool improves **Claude Code** on Windows and Linux by handling transient API errors gracefully, while still preserving the CLI’s native interactive behavior.

---

## Install

Clone and build from source:

```bash
cargo build --release
# binary will be at target/release/rusty-claude
````

Or install locally:

```bash
cargo install --path .
```

Copy the resulting binary somewhere on your PATH (e.g., `/usr/local/bin` or `C:\Users\Username\.local\bin`).

- If using Windows, ensure that `%USERPROFILE%\.local\bin` is in your path if you plan to store it there.

- You can do the following keypresses to get to your Windows Profile environment variables:

  - WIN key, type `edit env`, hit ENTER
  - An environment variables window should pop up showing User variables and System variables.
  - From there, edit your PATH to include the folder location of the binary.

---

## Usage

### Interactive (default when running from a TTY)

```bash
rusty-claude
```

This launches the real Claude CLI in interactive REPL mode, with retries only if the process fails.

### Non-interactive (piped JSON)

```bash
echo '{"model":"claude-3-5-sonnet-20240620","max_tokens":128,"messages":[{"role":"user","content":"Hello"}]}' \
  | rusty-claude -- --json
```

> Everything after `--` is forwarded to the **real** `claude` CLI.

### Pass arguments directly

```bash
rusty-claude -- --help
rusty-claude -- --version
```

### Tuning retries

```bash
rusty-claude \
  --max-retries 8 \
  --base-delay-ms 800 \
  --max-delay-ms 30000 \
  -- --json
```

### Environment overrides

You can also configure defaults via environment variables:

- `CLAUDE_SUPERVISOR_MAX_RETRIES`
- `CLAUDE_SUPERVISOR_BASE_MS`
- `CLAUDE_SUPERVISOR_CAP_MS`
- `CLAUDE_SUPERVISOR_PATTERNS` (pipe-separated regex patterns to detect retryable errors)

Example:

```bash
export CLAUDE_SUPERVISOR_MAX_RETRIES=10
export CLAUDE_SUPERVISOR_PATTERNS="Temporary failure|Upstream timeout"
```

### Forcing tee mode

By default, stdout/stderr are passed through natively in interactive TTY mode. If you want to capture/pipe them (e.g., for debugging), run:

```bash
rusty-claude --force-tee -- --json
```

### Custom CLI path

By default, `rusty-claude` looks for `claude` (Linux/macOS) or `claude.exe` (Windows) on your PATH. You can override with:

```bash
rusty-claude --cmd "/path/to/claude" -- --json
```

---

## Why not just use claude?

Well, Claude Code is great, but on Windows and Linux it sometimes throws recoverable API errors, at times repeatedly, like:

```
  ⎿  API Error: 500 {"type":"error","error":{"type":"api_error","message":"Overloaded"},"request_id":null}
  ⎿  API Error (Request timed out.) · Retrying in 1 seconds… (attempt 1/10)
  ⎿  API Error (Request timed out.) · Retrying in 1 seconds… (attempt 2/10)
```

**rusty-claude** acts as a **supervisor for Claude Code**: it wraps the official CLI, detects these transient failures, and retries with exponential backoff, while keeping the exact same usage and preserving interactive REPL behavior.
