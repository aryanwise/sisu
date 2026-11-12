sisu

It is is a model-agnostic, tool-augmented execution engine designed to help AI models complete complex tasks reliably from start to finish.

The model is interchangeable. The execution infrastructure is the product.

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
