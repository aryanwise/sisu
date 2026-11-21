# sisu

Sisu is being built as a model-agnostic, tool-augmented execution engine to help AI models complete complex tasks reliably from start to finish.

The current implementation is an Ollama-backed CLI for persistent chat, explicit workspace context, and validated project plans. Sandbox execution, generated file application, dependency installation, and verification are not implemented yet.

## Try the CLI

Use Rust 1.89 or newer and a reachable Ollama server with the selected model installed:

```sh
cargo build --release -p sisu-cli
./target/release/sisu
./target/release/sisu --help
./target/release/sisu --model qwen2.5-coder:7b chat "Explain this project"
./target/release/sisu plan "Build a small HTTP API"
```

In the interactive editor, enter a prompt and put `/send` on its own line. Use `/help` for commands, `/context add <path>` to include a workspace file, `/mode project` to plan, and Ctrl+C to cancel an active request. Sessions are saved under the selected workspace's `.sisu/` directory. List them with `sisu sessions list` and continue with `sisu --session <id>`. `--json` provides newline-delimited events for one-shot commands.

Configuration supports model, Ollama endpoint, timeout, and context limit, with CLI flags overriding environment and project/user JSON settings. See [CLI flow and commands](docs/sisu-cli-flow.md) for exact behavior, examples, configuration, storage, recovery, and current limitations. See [architecture](docs/architecture.md) for implemented and reserved components.

```sh
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Direction

The model is intended to be interchangeable. The execution infrastructure is the product.

That means sisu could work with:

- A local 7B model running on a normal consumer hardware.
- A 32B or 70B model running on an RTX GPU or Mac Studio.
- An inexpensive cloud model from OpenAI, Anthropic, or another provider.
- Our own specialized model trained specifically to work with sisu's orchestration and tools.
  And the tools wouldn't have to be limited to coding. Design, CAD, documents, research, security, spreadsheets, and other domains could eventually become part of the same ecosystem.

This ain't another AI coding assistant.

It started with a fundamental question:
Can a relatively small language model perform complex software engineering tasks reliably if we give it access to the right tools, knowledge, and execution environment instead of relying entirely on what it learned during training?

That's the central hypothesis behind sisu.

1. The Problem You're Trying to Solve
   Small coding models, such as Qwen2.5-Coder 7B, have several limitations:

- Limited knowledge: They may not know newer libraries or unfamiliar APIs.
- Hallucinations: They sometimes invent nonexistent functions or incorrect syntax.
- Limited context: They struggle when processing large codebases.
- Long-task failures: They may lose track of requirements while building complex projects.
- Weak self-correction: They may repeatedly generate incorrect code without understanding why it fails.

Our approach is to compensate for these weaknesses through engineering rather than simply increasing model size.
