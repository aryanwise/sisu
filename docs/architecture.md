# Sisu v1 structure

The workspace retains two crates: `sisu-cli` handles terminal input and the application entry point; `sisu-core` holds the existing model interaction, planning state, persistence, and milestone execution.

## Existing implementation

- `sisu-cli/src/input.rs`: input types, mode selection, command parsing, and prompt collection.
- `sisu-cli/src/main.rs`: application entry point and existing session flow.
- `sisu-core/src/models.rs`: project, milestone, and task status models.
- `sisu-core/src/llm/mod.rs`: existing LLM configuration.
- `sisu-core/src/llm/ollama.rs`: existing Ollama model call.
- `sisu-core/src/llm/prompts.rs`: existing planning and execution prompts.
- `sisu-core/src/state.rs`: existing plan parsing and persistence.
- `sisu-core/src/orchestrator.rs`: existing planning helper and milestone execution.
- `sisu-core/src/lib.rs`: module declarations and public re-exports.

Function bodies, prompts, configuration defaults, state format, and execution behavior are preserved. Model fields used by the orchestrator have crate-level visibility so sibling modules can access them.

## Reserved components

`workspace/`, `languages/`, `syntax/`, `sandbox/`, and CLI `display.rs` are structural placeholders only. No file materialization, isolation, verification, retries, parser loading, or language adapters have been added. Existing print statements remain in their original functions.

Language adapters will select project-specific tooling. Syntax support will select grammars and queries per language/file. Sandbox backends will enforce execution restrictions independently of the project language. Rust and Python filenames are initial placeholders; they do not restrict future language support.

The core `tests/` directory is reserved for future integration tests; no new tests are included in this refactor. Task workspace/state/log directories will be created by future runtime implementation, rather than checked in as sample project data.
