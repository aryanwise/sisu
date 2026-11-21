# Sisu CLI flow

The CLI currently supports persistent conversations, explicit file context, and validated project plans through Ollama. It does not execute generated code, install dependencies, apply file changes, or provide sandbox isolation. Execution-related commands report that their capabilities are unavailable.

## Build and start

Use Rust 1.89 or newer (edition 2024 and `std::fs::File::try_lock` support):

```sh
cargo build --release -p sisu-cli
./target/release/sisu
```

The examples below assume `target/release` is on `PATH`. Ollama must be reachable and the selected model installed for chat, planning, model discovery, and provider diagnostics. Read-only local commands do not require a running provider.

```sh
sisu --help
sisu -C ./my-project --model qwen2.5-coder:7b
sisu doctor
```

`-C`/`--workspace` selects an existing directory; it does not create one. Without a subcommand, terminal stdin starts the interactive editor. Without a subcommand and with piped stdin, Sisu sends the input as a one-shot chat request. Stdin prompts are limited to 1 MiB and must be UTF-8.

## Interactive input

```text
sisu [my-project | chat | qwen2.5-coder:7b]> Explain the current architecture.
… /send to submit> /send
```

The session remains open after each response. The prompt shows workspace name, mode, and current model. Plain text is accumulated until `/send` appears on its own line. Enter adds another input line; Alt+Enter inserts a newline inside the editor. Indentation and trailing spaces are preserved.

Commands start with `/` at the beginning of a dedicated line. `/chat <prompt>` and `/plan <goal>` send immediately. A trailing word such as `Explain /exit` remains ordinary text. Bracketed multiline pastes and lines inside fenced code blocks remain literal. Use `//exit` to include the literal line `/exit`. Command arguments accept quotes and backslash escapes; they are tokenized without executing a shell or expanding `$HOME`, substitutions, or globs.

A pending draft must be submitted with `/send` or discarded with `/cancel` before another command runs. Invalid commands retain the draft. `/cancel` and Ctrl+C discard the draft while editing. EOF/Ctrl+D exits cleanly, discarding any unsent draft and retaining the saved conversation.

During a provider request, Ctrl+C cancels that request and returns to the interactive prompt. The editor is inactive while generation runs, so typing `/cancel` cannot interrupt an active request. This is not a full-duplex terminal. Partial streaming output may already have been displayed and journaled; it is not committed as an assistant message. The user message remains saved. Cancellation drops the local request; it does not guarantee that a remote Ollama server immediately stops inference. Cancellation in a one-shot invocation returns exit code 130; command errors return 1, argument-parsing errors return 2, and success returns 0.

Tab completes commands, saved session IDs, known model names after `/models`, selected context removals, and workspace paths for `/context add`. Input history and search are provided by Reedline, with up to 1,000 history entries in `.sisu/history.txt`.

## Commands

Shell subcommands and interactive slash commands share the same typed command catalog and handlers. For example, `sisu context add src/main.rs` and `/context add src/main.rs` perform the same operation, though a shell invocation has its own session unless `--session` is supplied.

| Interactive command | Behavior |
| --- | --- |
| `/help [command]` | Show generated command help and arguments. Shell forms: `sisu help [command]`, `sisu --help`. |
| `/chat <prompt>` | Send a conversational request, including saved conversation history. |
| `/mode [chat\|project]` | Inspect or persist the session mode. Plain submitted text chats in `chat` mode and generates a plan in `project` mode. |
| `/model [name]` | Inspect or change the model for the current invocation. |
| `/models` | Query installed models at the configured Ollama endpoint and update completions. |
| `/config [show]` | Show effective settings and their origins. |
| `/status` | Show session ID, workspace, mode, model, message count, last status/error, plan presence, and execution availability. |
| `/plan <goal>` | Generate and validate a project plan. |
| `/plan` | Display the active session's saved plan. |
| `/plan validate` | Validate the saved plan without calling the model. The literal goal `validate` is reserved for this operation. |
| `/context [show]` | Show explicitly selected files, byte counts, and approximate input size. |
| `/context add <path>` | Add a readable UTF-8 file within the workspace. |
| `/context remove <path>` | Remove a selected file; its saved relative path also works after deletion. |
| `/sessions [list]` | List saved sessions in the selected workspace. |
| `/resume <id>` | Save the current session and switch to a saved session. |
| `/new` | Save the current session and start a fresh conversation in chat mode. |
| `/clear` | Clear the terminal while retaining conversation history; skip terminal escape sequences for redirected/JSON output. |
| `/cancel` | Discard an input draft; use Ctrl+C during generation. |
| `/exit` | Save and exit. |
| `/doctor` | Inspect configuration and provider connectivity/model availability. Storage write errors are checked on actual saves, not by a write probe. |
| `/sandbox [status]` | Report unavailable sandbox and execution capabilities. |
| `/run [id]`, `/check`, `/retry [id]` | Report unavailable execution/verifier support. |
| `/diff`, `/checkpoint`, `/restore [id]` | Report unavailable workspace snapshot support. |
| `/send` | Submit an interactive draft; editor-only, with no shell subcommand. |

Model and endpoint settings are resolved for each process. `/model` changes that process's configuration; it does not rewrite configuration files or become part of the saved session. Mode, messages, plan, and selected context paths do persist.

## One-shot use and resuming

```sh
sisu chat "Explain dependency injection"
printf 'Explain this error\n' | sisu chat -
printf 'Build a small HTTP API\n' | sisu plan -
sisu plan "Build a small HTTP API"
sisu sessions list
sisu --session <id> plan validate
sisu --session <id> chat "Use the previous answer to suggest next steps"
sisu --session <id>
```

`--session <id>` silently restores the session before running the requested command, or before entering the interactive editor; it adds no startup status record to command output. `sisu resume <id>` is itself a one-shot command: it restores and prints session status, then exits. To continue interacting, use `sisu --session <id>` from a terminal. To display a saved plan in another process, use `sisu --session <id> plan`.

Explicit `chat`/`plan` commands choose the operation directly, regardless of the persisted interactive mode. An invocation creates a session lazily when it first needs one; commands such as help or status can run without creating a conversation.

Global flags work before or after the subcommand:

| Flag | Purpose |
| --- | --- |
| `-C`, `--workspace <path>` | Workspace and local state/configuration root. |
| `--session <id>` | Restore a saved workspace session first. |
| `--model <name>` | Override the active model. |
| `--endpoint <url>` | Override the Ollama HTTP(S) endpoint. |
| `--timeout <seconds>` | Total provider-request deadline, including response streaming. |
| `--context-limit <tokens>` | Model context window and approximate input-size guard. |
| `--json` | Emit newline-delimited JSON events for one-shot or piped use. Interactive `--json` is rejected. |
| `-q`, `--quiet` | Suppress started/delta/finished events while retaining final output, errors, and cancellations. |
| `--no-color` | Disable editor ANSI colors; `NO_COLOR` is also respected. |

Ordinary output goes to stdout; human-readable progress and errors go to stderr. `--json` emits tagged events to stdout, including errors. With `--json`, argument-parsing errors also emit an NDJSON failure record and exit with code 2. `--help` and `--version` retain their normal CLI formatting. For one final response without stream deltas, combine `--json --quiet`:

```sh
sisu --json --quiet chat "Explain ownership"
```

Example event shapes:

```json
{"event":"started","operation":"chat"}
{"event":"delta","text":"Ownership"}
{"event":"output","kind":"response","data":"Ownership ..."}
{"event":"finished","operation":"chat","elapsed_ms":420}
```

Full JSON output includes both deltas and the final complete response. Structured plans are emitted only after a complete response has passed validation; partial plan JSON is not presented as an accepted plan.

## Configuration

Settings resolve in this order, with later sources overriding earlier ones:

```text
defaults → user config → workspace config → SISU_* environment → CLI flags
```

User configuration is `$XDG_CONFIG_HOME/sisu/config.json`, falling back to `$HOME/.config/sisu/config.json`. Workspace configuration is `<workspace>/.sisu/config.json`. Missing files are allowed; invalid JSON, unknown keys, unreadable files, and invalid values produce actionable errors.

```json
{
  "model": "qwen2.5-coder:7b",
  "endpoint": "http://localhost:11434",
  "timeout_secs": 120,
  "context_limit": 8192
}
```

All fields are optional in configuration files. These example values are the defaults. Environment variables are `SISU_MODEL`, `SISU_ENDPOINT`, `SISU_TIMEOUT_SECS`, and `SISU_CONTEXT_LIMIT`. The model must be nonempty; timeout and context limit must be positive integers. Endpoints must use HTTP(S), without embedded credentials, query parameters, or fragments. `/config show` includes the origin of each effective setting. No authentication-secret setting is implemented.

## Context and plan boundaries

File context is opt-in. Sisu does not automatically index the repository or apply `.gitignore` rules to context. Selected paths are canonicalized and must resolve to regular files inside the workspace; `.sisu` state/configuration files are excluded. Files must be UTF-8, at most 256 KiB each, and at most 1 MiB combined. Contents are read again on each request, so selection is not a file snapshot.

Conversation history, selected file contents, and planning instructions are assembled into the model request. A rough `ceil(UTF-8 bytes / 4)` estimate rejects requests exceeding the configured context limit rather than silently truncating history. This is not exact token counting and does not reserve output tokens or account precisely for protocol overhead. `/context` shows file/history estimates; the actual request guard also includes injected system text.

Plan validation checks required text, 1–4,096 milestones, nonempty unique IDs, completion criteria, missing/repeated/self dependencies, and dependency cycles. Dependency ordering in the JSON does not need to be topological. Newly generated plans have milestone execution metadata reset to `Pending`, zero attempts, no artifacts, and no last error. Loaded plans preserve their saved statuses. Validation establishes structural validity, not correctness or successful execution. A declared `check_command` is data and is never run. Registry dependencies remain declarations and are never installed.

When a new plan is invalid, the previous saved plan is retained. The complete invalid model response is written to the session's `last-invalid-plan.txt` for inspection. Network, API, timeout, malformed response, and state-write failures are reported without treating the request as successful. Requests are not retried automatically.

## Session state and recovery

```text
<workspace>/.sisu/
├── config.json                 # optional workspace configuration
├── history.txt                 # interactive editor history
└── sessions/<id>/
    ├── session.json            # schema version 1; authoritative saved state
    ├── session.lock            # OS advisory lock held while session is open
    ├── events.jsonl            # append-only emitted events, with timestamp_ms
    └── last-invalid-plan.txt   # present after an invalid plan response
```

Session state includes workspace identity, mode, messages, plan, selected context paths, timestamps in Unix milliseconds, last operation status, and last error. IDs are validated before loading; workspace ownership, schema version, and saved plans are checked on resume. Session-state and lock leaves must be regular files; existing or dangling symlinks and special files are rejected. State is saved through a same-directory temporary file, file synchronization, and atomic rename; directory synchronization is also used on Unix. A second Sisu process cannot open the same session for writing. The operating system releases the lock when its handle closes or the process dies; the lock file's presence alone does not mean a session is busy.

Session operation statuses are `idle`, `chat`, `plan`, `planned`, `cancelled`, `failed`, and `interrupted`. A session found in `chat` or `plan` on resume is marked `interrupted`; no partial assistant response is accepted. Successful planning means a plan was saved, not that its milestones were completed. The model retains milestone status types for future execution, including `Generated`, but the CLI never verifies completion.

The event journal uses the same typed events as terminal/JSON rendering. It is an append-only diagnostic record, not a transactional replay mechanism; the saved session snapshot remains authoritative. Journal write failures are surfaced. It may contain streamed text and full responses. State is stored as plaintext. This repository ignores `.sisu/sessions/` and `.sisu/history.txt`, while `.sisu/config.json` may be committed as shared project configuration. When using Sisu in another workspace, add equivalent ignore rules there for local sessions and history.

The standalone core `persist_plan` API still writes `milestones.json` atomically for library callers; the CLI stores its plan within `session.json`. That library helper also uses an OS-backed lock. Its persistent lock file is not a stale-lock indicator and should not be deleted to unlock it.

## Internal flow and checks

```text
shell arguments / interactive slash command
    → shared Command enum (Clap)
    → App dispatch + current SessionHandle
    → context construction + approximate input guard
    → Ollama ModelClient (deadline; caller-controlled cancellation)
    → streamed chat / complete structured plan + validation
    → atomic session save
    → typed events → terminal or NDJSON + session journal
```

`commands.rs` owns the command catalog and argument tokenizer. `input.rs` owns editing and draft parsing. `app.rs` coordinates state and operations. `config.rs` resolves settings; `session.rs` owns snapshots and locks; `display.rs` renders events. Core transport and plan validation remain independent of terminal formatting.

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Tests cover shared shell/slash parsing, pasted/fenced input, configuration precedence, session locking/recovery validation, explicit context bounds, dependency graph validation, persistence, provider errors/timeouts, and streamed response handling. Real-binary tests use local mock HTTP servers to check piped input, history continuation, invalid-plan recovery, disabled execution, and Unix SIGINT cancellation; a live model is not required. The provider is currently Ollama only. Full-duplex input, automatic retries, exact tokenizer accounting, cloud providers, workspace snapshots, code application, verification, and isolation remain future work.
