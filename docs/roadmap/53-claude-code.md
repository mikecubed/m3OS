# Phase 53 - Claude Code

## Milestone Goal

Claude Code — Anthropic's AI coding agent — runs natively inside m3OS. An AI agent
operates on an operating system it helped design, implement, and document. This is
the ouroboros milestone.

## Learning Goals

- Understand the full software stack required for a modern AI-powered CLI tool:
  Node.js runtime, HTTPS/TLS, DNS, terminal interaction, subprocess management.
- See how all previous phases combine into a single working system capable of
  running a complex real-world application.

## Feature Scope

### Claude Code Installation

```bash
# With npm from Phase 52
$ npm install -g @anthropic-ai/claude-code

# Set API key
$ export ANTHROPIC_API_KEY="sk-ant-..."

# Run
$ claude
```

### What Claude Code Does Inside m3OS

Claude Code reads the local codebase, sends context to the Anthropic API over HTTPS,
receives instructions, and executes tools (file reads/writes, shell commands, git
operations). Inside m3OS, it can:

- Read kernel source code (`kernel/src/`)
- Propose and implement new kernel features
- Compile C programs with TCC or Clang (Phase 50)
- Run Python scripts (Phase 50)
- Use git for version control (Phase 50/51)
- Create GitHub PRs with `gh` (Phase 51)
- Run tests inside the OS

### Network Path

```
Claude Code → Node.js https → TLS 1.3 → TCP → virtio-net → QEMU NAT → api.anthropic.com
```

QEMU's user-mode networking provides NAT for outbound HTTPS connections. DNS resolves
`api.anthropic.com` via the QEMU gateway forwarder.

### Prerequisites Integration

This phase has no new kernel work — it is purely integration of all previous phases:

| Component | Provided by |
|---|---|
| Node.js runtime | Phase 52 |
| npm | Phase 52 |
| HTTPS to Anthropic API | Phase 42 (TLS) + Phase 51 (DNS) |
| File I/O | Phase 24 (persistent storage) |
| Process spawning (shell commands) | Phase 11 (process model) |
| Pipes (command output capture) | Phase 14 (shell and tools) |
| Terminal UI (colors, cursor) | Phase 22 (TTY) + Phase 29 (PTY) |
| git | Phase 50 + Phase 51 |
| Environment variables | Working since early phases |

See [Claude Code roadmap](../claude-code-roadmap.md) for the full dependency graph
and architecture diagrams.

## Dependencies

- **Phase 50** (Cross-Compiled Toolchains) — git for code understanding
- **Phase 51** (Networking and GitHub) — HTTPS, DNS, gh
- **Phase 52** (Node.js) — runtime + npm

## Acceptance Criteria

- [ ] `npm install -g @anthropic-ai/claude-code` succeeds.
- [ ] `claude --version` prints the version.
- [ ] `claude "what files are in /usr/src/"` lists files.
- [ ] `claude "write a hello world in C and compile it with tcc"` creates and
      compiles a program.
- [ ] Claude Code can read kernel source files.
- [ ] Claude Code can make git commits.
- [ ] Claude Code can create GitHub PRs via `gh`.

## Deferred Items

- **MCP servers** — Claude Code's Model Context Protocol for extended tool use.
  Would need additional Node.js packages.
- **Claude Code hooks** — custom automation hooks; require shell script support.
- **Multi-user Claude Code** — each user with their own API key and workspace.
- **Offline mode** — Claude Code requires internet access to the Anthropic API.

## The Meta Moment

When Claude Code runs on m3OS, we achieve something remarkable: an AI agent running
on an operating system it helped design, implement, and document. Claude Code can
then:

- Read its own kernel source code
- Propose and implement new kernel features
- Compile C programs with Clang (Phase 50)
- Run tests inside the OS it's running on
- Commit changes to the git repo that contains its own OS
- Create PRs for its own improvements

The agent becomes a native citizen of the system it built.
