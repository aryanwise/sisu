# Sisu v1 architecture

The workspace contains two crates. `sisu-cli` owns terminal interaction, command dispatch, configuration, and persistent conversations. `sisu-core` owns Ollama transport, plan/data contracts, validation, and standalone plan persistence. See [CLI flow and commands](sisu-cli-flow.md) for usage and recovery behavior.

## Implemented CLI foundation

| Module | Responsibility |
| --- | --- |
| `sisu-cli/src/main.rs` | Parse global arguments, select interactive/one-shot operation, restore `--session`, handle stdin and process exit. |
| `sisu-cli/src/commands.rs` | Shared typed shell/slash command catalog, generated help, completion names, shell-free argument tokenization. |
| `sisu-cli/src/input.rs` | Reedline editing/history/completion, literal-safe multiline drafts, `/send`, EOF, and editing cancellation. |
| `sisu-cli/src/app.rs` | Dispatch commands, select operation by mode, assemble context/history, coordinate cancellation, save results, journal events. |
| `sisu-cli/src/config.rs` | JSON configuration, environment/flag precedence, value validation, and source reporting. |
| `sisu-cli/src/session.rs` | Versioned session snapshots, atomic saves, workspace/ID validation, and OS-backed single-writer locks. |
| `sisu-cli/src/display.rs` | Shared events rendered as terminal output or newline-delimited JSON. |

The editor owns input while idle; the async operation owns Ctrl+C during requests. Chat streams text. Structured planning waits for a complete response, validates it, resets newly generated milestone execution metadata to pending, and saves the accepted plan. It does not execute milestones. Session snapshot state is authoritative; the append-only event journal is diagnostic rather than an event-replay database.

Session and history files live under `<workspace>/.sisu/`. This repository ignores `.sisu/sessions/` and `.sisu/history.txt`; project `.sisu/config.json` may be committed. Other workspaces need equivalent ignore rules for their local conversation state. Configuration is resolved afresh for each process; session mode, messages, plan, and explicitly selected context paths persist. Model changes made with `/model` last for the current process only. Context is read directly from selected workspace text files on each request, with file/aggregate bounds and an approximate input-size guard. There is no repository index or workspace snapshot yet.

## Core contracts

- `llm/mod.rs` defines model options and serializable role/message types.
- `llm/ollama.rs` implements `ModelClient` using the Ollama HTTP API, including model listing, complete/streamed chat, configurable endpoint/deadline, bounded responses, and typed errors. Dropping the request future allows caller-controlled cancellation. Ollama is the only implemented provider.
- `models.rs` exposes project, milestone, dependency, and generated-file data structures.
- `state.rs` parses and validates plans, including their full dependency graph. `persist_plan` and `load_plan` support standalone `milestones.json` files for library consumers; saves are atomic and use an OS-backed writer lock. The CLI instead embeds the current plan in its session snapshot.
- `orchestrator.rs` retains a planning helper and an explicit `ExecutionUnavailable` boundary. `execute_milestones` returns that error without invoking the model, writing generated files, or changing milestone state.

`TaskStatus::Generated` expresses that output exists without verification. Neither model output nor a valid plan proves completion. The CLI resets execution metadata on a newly accepted model plan and does not advance milestones to `Completed`.

## Dependency and file declarations

`ProjectPlan.dependencies` contains registry package names, separate from milestone `depends_on` IDs. Missing dependency lists default to empty for compatibility. Package declarations are not installed or resolved.

The executor prompt describes `GeneratedPayload { files, add_dependencies }`. Its typed file/payload contracts reject unknown fields. Payload application, package validation, language adapters, and package-manager invocation are not connected to the CLI. Future dependency installation must pass through execution isolation because package installation/build scripts can execute code. Missing imports alone do not justify an install.

## Reserved execution components

`workspace/`, `languages/`, `syntax/`, and `sandbox/` remain reserved module boundaries. There is no file materialization, process execution backend, isolation, verification, repair loop, workspace checkpoint, or Tree-sitter parser integration. `/sandbox status` reports this explicitly; execution and snapshot commands return capability errors.

Language adapters will select tooling per project. Syntax support will select grammars/queries per file. Sandbox backends will enforce execution restrictions independently of project language. Existing Rust/Python placeholder filenames do not restrict future language support.

## Verification

Run `cargo test --workspace`, `cargo fmt --all -- --check`, and `cargo clippy --workspace --all-targets -- -D warnings`. Core tests cover schema compatibility, dependency validation, atomic persistence, disabled execution, and transport behavior. CLI tests cover command/input contracts, configuration, sessions, and explicit file context. Transport tests use local mock HTTP servers rather than requiring a live model.
