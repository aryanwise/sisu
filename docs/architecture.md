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

Model fields used by the orchestrator have crate-level visibility so sibling modules can access them.

## Dependency declarations

`ProjectPlan.dependencies` stores registry package names for the chosen project language. Missing lists default to empty for compatibility with older plans. Each milestone prompt receives the declared dependencies, without claiming that they have been installed.

The executor prompt requests `GeneratedPayload { files, add_dependencies }` instead of command actions. The optional `add_dependencies` list defaults to empty. `GeneratedPayload` and `FileWrite` define the typed JSON contract and reject unknown fields when deserialized. Package names remain unvalidated declarations until a language adapter validates them.

Execution still prints the raw model response; payload parsing/application and dependency installation are not wired into the orchestrator yet. No package manager is invoked by this change. Future installation must also run in isolation because package managers may execute package install/build scripts. Missing imports alone are not proof that a registry package should be installed.

## Reserved components

`workspace/`, `languages/`, `syntax/`, `sandbox/`, and CLI `display.rs` are structural placeholders only. No file materialization, isolation, verification, retries, parser loading, or language adapters have been added. Existing print statements remain in their original functions.

Language adapters will select project-specific tooling. Syntax support will select grammars and queries per language/file. Sandbox backends will enforce execution restrictions independently of the project language. Rust and Python filenames are initial placeholders; they do not restrict future language support.

The core `tests/dependencies.rs` integration tests cover dependency schema compatibility, persistence, and execution payload validation. Task workspace/state/log directories will be created by future runtime implementation, rather than checked in as sample project data.
