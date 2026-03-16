# CLAUDE.md

## Rules

- **DON'T BOTHER WITH BACKWARD COMPATIBILITY.**
- Always thoroughly study all existing code relevant to your current task before offering changes.
- **NEVER USE EMOJIS** in code or commits.
- Always use latest dependency versions possible.
- Always run code formatters before committing (`cargo fmt` for backend, `npm run check` and `npx prettier --plugin prettier-plugin-svelte --write "src/**/*.{ts,svelte}" 2>&1` for frontend).
- Always run `cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings` before committing and fix any warnings.
- Always omit Claude signature when writing commit messages.
- Always follow Rust idioms and best practices.
- Always follow the idioms and best practices of the project dependencies (e.g Tokio, Tauri, etc).

## Task Continuity (anti-compaction-drift)

Before starting any multi-step task, create `docs/current-task.md` with:
- **Goal**: description of what we are trying to accomplish
- **Plan**: numbered list of steps
- **Status**: which step is in progress, which are done
- **Files in flight**: list of files currently modified but not yet committed
- **Blocking issues**: anything that is unresolved or needs a decision

Update `docs/current-task.md` after each step completes. When a step is done, mark it done and advance the status line. Delete the file after the final commit.

At the start of every response during a multi-step task, read `docs/current-task.md` if it exists and state in one line where we are (e.g. "Resuming: step 3/5 — wiring up the command handler"). This ensures compaction cannot cause drift.

If the file is missing but the working tree has uncommitted changes, stop and reconstruct the task file by inspecting `git diff --stat` and `git status` before proceeding.

## Commands

- When the user says "review and commit", this means review ALL the uncommitted changes with git diff, then commit.
- When the user says "review and report", this means re-inspect ALL the code relevant to the current task and revise your list on current options.
