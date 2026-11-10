# Rust engineering guide

These rules shape the Rust you write or change; they are not a checklist of things to add. What the request specifies wins, then the project's conventions, lints, edition and `rust-version`. Don't add lint configuration, dependencies, wrapper types, crates or hardening the task doesn't need, and don't rewrite working code to match this guide: report what you noticed instead. A bullet that starts with a context (`Workspace:`, `Windows:`) applies only there.

## Crates and dependencies

- Workspace: dependencies point one way. To reach one item in a large crate, or to share types between two components, move them into a lower crate both use instead of depending on the large crate. Break a would-be cycle with a trait in the lower crate that the binary implements and passes in.
- Workspace: a new binary is its own bin-only package that nothing depends on, not another `[[bin]]` on a library package whose dependencies it would then link. It owns `main`, `#[global_allocator]` and feature selection; libraries only forward features. Don't relocate or rename existing binary targets, or put existing public modules behind a new feature, unless asked. A git-hash stamp belongs in a binary package's `build.rs`, never a library's, so libraries don't rebuild on every commit.
- Workspace: split a crate only to cut rebuilds, parallelize the build, break a cycle or keep a heavy dependency away from consumers that don't need it, never for length. Re-export moved items at their old paths so callers compile unchanged; reserve optional `dep:` features for add-on integrations, not for code that already ships by default.
- Data and wire-type crates take no async runtime, HTTP client or I/O dependency; convert transport errors into owned data at the boundary.
- Workspace: declare each dependency once in `[workspace.dependencies]`, with `default-features = false` there if any member needs defaults off; edition 2024 rejects a member-level opt-out, and member features only add.
- Workspace: share test helpers behind a non-default `test-support` feature (`#[cfg(any(test, feature = "test-support"))]`) that dependents enable in `[dev-dependencies]`; a dependency's `#[cfg(test)]` code is never built for your tests.

## Types and zero-cost abstractions

- In the wire, config and storage types you define, values with invariants (ids, sizes, limits) get a validated type that decoding also goes through: a private-field newtype with one fallible constructor and `#[serde(try_from = "Raw")]`, or `NonZero<T>` when nonzero is the whole invariant.
- Dispatch statically by default: closed sets are enums; `dyn` is for open registries, stored heterogeneous callbacks and test seams. Write async trait methods as `fn op(&self) -> impl Future<Output = T> + Send`, not `#[async_trait]`; where a registry needs `dyn`, add a boxed-future twin trait with a blanket impl and erase only there.
- Let the compiler flag new cases: no `_` arm where each variant of your own enum needs a decision; destructure `let Self { a, b, c: _ } = self;` in hand-written `PartialEq`, `Hash` or merge code; relate constants with `const _: () = assert!(A > B, "why");`.
- Share immutable data cloned to many owners as `Arc<str>`/`Arc<[T]>`/`Bytes`, and return `Cow<'_, str>` from transforms that usually change nothing, behind a cheap pre-check.
- Where JSON handling is measurably hot or payloads run to megabytes, don't build a `serde_json::Value` just to validate, peek or measure: use `IgnoredAny`, borrowed structs with `&RawValue` fields, or `to_writer` into a byte-counting `io::Write`.
- In per-frame or per-item loops, reuse buffers (`clear()`, `mem::take`), parse streaming input incrementally from resumable state, and key caches on exactly the inputs that change the result.

## Errors

- APIs whose callers branch on failures return their own typed error enum, not `io::Result`, `String`, a boxed error or a dependency's error type. Variants carry what the caller acts on (path, id, status, retry-after) and keep the cause in a `#[source]`/`#[from]` field that the message does not repeat.
- Inside the process, never flatten a cause into text (`e.to_string()`, `anyhow!("…: {e}")`, `Result<_, String>`), and branch on types, never on message text.
- When retrying, classify on the error type: `is_retryable()` matching every variant, with no `_` arm. Retry only idempotent operations, with capped jittered backoff and a capped `Retry-After`; give each attempt its own timeout inside one overall deadline that is computed once and passed down (`timeout_at`).
- Where an error is serialized for another process, an API client or an LLM, map it there to a stable kind tag and an actionable message; the internal cause chain goes to logs (`{e:#}` for `anyhow`, tracing `?e` otherwise), not to them.

## Async and concurrency

- A future can be dropped at any `.await`, and `?` returns early: restore, re-queue or decrement state in a `Drop` guard created before that point, not in code after it. In `select!` loops, race only cancel-safe futures (never a fresh `read_line`, `read_exact` or `write_all`), with the cancellation branch first under `biased;`.
- Dropping a `CancellationToken` does not cancel it: keep a `drop_guard()` so that an early return stops the work you spawned. Own tasks in a `JoinSet` (aborts on drop) or a `TaskTracker` plus a token.
- `spawn_blocking` work can't be aborted, and a `timeout` around it only stops waiting: give long work a cancel flag to check. Avoid `block_in_place`, which panics on a current-thread runtime, the `#[tokio::test]` default.
- In long-running programs, bound whatever buffers data from outside the process (channels, line and body lengths, captured child output, sizes a peer or an LLM chooses), with limits the caller can set. Decide what a full queue means (wait, drop or fail), and in a fan-out never let one slow consumer block the others. Treat `broadcast` `Lagged` as data loss and resync; wait for state with `watch`, not check-then-`Notify`.

## Processes, files and panics

- Children that a long-running program supervises (agents, servers, CI runners) get their own process group (`process_group(0)`; a Job Object on Windows) and are killed as a group, because `kill_on_drop` reaches only the direct child. Null any stdin they don't read (`Stdio::null()`) and disable prompts and pagers (`GIT_TERMINAL_PROMPT=0`, `GIT_PAGER=cat`).
- `Drop` cleanup is best-effort, synchronous and non-blocking, works without a runtime and never panics. Resources whose release can fail also offer `close(self) -> Result`.
- Replace state files atomically: a temp file in the same directory, write, `sync_all`, then rename (`tempfile::NamedTempFile::new_in` and `persist`).
- With `panic = "abort"`, `Drop` and `catch_unwind` don't run on panic, and tests still unwind, so they won't show it: do must-run cleanup (restoring a raw terminal, killing child groups) in a panic hook that chains the previous one.
- In long-running programs, use `get`, `split_first`, `split_at_checked` or slice patterns instead of indexing, unless the index was just bounds-checked. `expect`/`unreachable!` messages state the invariant that rules failure out; failures that can happen return errors.
- Windows: canonicalize with `dunce::canonicalize`; `std` returns `\\?\` paths that break other tools and path equality.

## Serde and wire formats

- For data other versions read or write, add fields as `#[serde(default)]`, keep unknown keys when rewriting a file (a `#[serde(flatten)]` map), and give string and internally tagged enums a catch-all instead of failing the whole message (`#[serde(other)] Unknown`). `#[non_exhaustive]` is for published APIs, not wire tolerance.
- Pick strictness by source: `deny_unknown_fields` for same-version internal messages; tolerate unknown keys in user config.

## Tests and lints

- Test time-dependent code with an injected clock or `#[tokio::test(start_paused = true)]` (tokio `test-util`), never real sleeps. When a fast or incremental path replaces a full recompute, assert identical output against the full one.
- Silence a lint with `#[expect(lint, reason = "..")]` on the narrowest item, never a bare or crate-level `#[allow]`.
- Only when asked to set up lint policy: ban a raw API that must go through a wrapper with clippy `disallowed-methods` (the `reason` names the wrapper); deny `clippy::indexing_slicing` with `allow-indexing-slicing-in-tests = true`; deny `unwrap_used`, `expect_used` and `panic` under `cfg_attr(not(test), ..)` in shutdown and `Drop` code; enable `clippy::undocumented_unsafe_blocks`. Clippy reads only the nearest `clippy.toml`.

---

Copied from [agent-kit](https://github.com/acking-you/agent-kit/blob/afe17f0b4217959e069a09d0191459dbb227b67f/prompts/language-guides/rust/2026-11-09-RUST.md) at `afe17f0`. Where it and [AGENTS.md](../AGENTS.md) differ, AGENTS.md wins.
