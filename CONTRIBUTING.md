# Contributing to toon-mcp

## Prerequisites

- Rust **1.93.0** (set via `rust-toolchain.toml`; `rustup` picks this up automatically)
- A working `cargo` installation

## Running the test suite

```sh
cargo test --workspace
```

Benchmarks live in `crates/toon-mcp-bench` and are excluded from `cargo test` by default. Run them explicitly with:

```sh
cargo bench --package toon-mcp-bench
```

## Running the full lint gate

The CI clippy invocation is the canonical lint gate. Run it locally before pushing:

```sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Run the formatter check the same way CI does:

```sh
cargo fmt --check
```

Apply formatting in place with:

```sh
cargo fmt
```

## Branch and PR model

- Work against `master`. Branch off `master` for every change.
- Branch names: `feat/<short-description>`, `fix/<short-description>`, `chore/<short-description>`.
- Open a pull request against `master`. CI also runs on `main` while both branch names exist, but `master` is the active development branch.
- All CI checks (fmt, clippy, test, doc, audit, semver where applicable, coverage >= 75%) must pass before merge.
- Squash-merge is preferred to keep the history linear.
- Update `CHANGELOG.md` under `[Unreleased]` as part of the PR.

## Semver policy

This crate follows [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html).

| Change type                                                                              | Version bump |
| ---------------------------------------------------------------------------------------- | ------------ |
| New MCP tool exposed to callers                                                          | **minor**    |
| Removal or rename of an existing MCP tool or its parameters                              | **major**    |
| Change to a public Rust API (`pub` type, function, or trait) that breaks downstream code | **major**    |
| Backward-compatible addition to a public Rust API                                        | **minor**    |
| Bug fix, performance improvement, internal refactor, dependency update                   | **patch**    |
| Breaking change to configuration env-var names or semantics                              | **major**    |

Pre-1.0 note: the project is currently at `0.1.x`. Until `1.0.0` is tagged, minor bumps may carry breaking changes per semver §4, but we aim to avoid this.

## Compatibility Policy

- Supported Rust: the exact toolchain pinned in `rust-toolchain.toml` (`1.93.0` at the time of writing).
- Supported release artifacts: Linux GNU and macOS on `x86_64` and `aarch64`, matching `.github/workflows/release.yml`.
- Supported MCP clients: stdio MCP clients that can launch a local binary and pass environment variables, including opencode and Claude Desktop.
- API stability: before `1.0.0`, MCP tool schemas, public Rust APIs, and environment variable semantics may change in a minor release. Document breaking changes in `CHANGELOG.md`.

## Dependency pinning

All workspace dependencies in `Cargo.toml` use exact (`=`) version pins. When adding or updating a dependency:

1. Pin the version with `=` in `[workspace.dependencies]`.
2. Run `cargo test --workspace` to confirm nothing broke.
3. Note the bump in `CHANGELOG.md`.

Dependabot opens weekly PRs for patch-level updates; review and merge them promptly to avoid accumulation.

## Fuzz Testing

The `fuzz/` directory contains a `cargo-fuzz` harness for the parser surface
(JSON, JSONL, CSV/TSV, and the format-detection entry point). Fuzzing requires
the nightly Rust toolchain and is not part of CI.

```bash
# One-time setup
rustup install nightly
cargo install cargo-fuzz

# Run a target (Ctrl-C to stop)
cd fuzz
cargo +nightly fuzz run detect_and_parse
cargo +nightly fuzz run json_parse
cargo +nightly fuzz run jsonl_parse
cargo +nightly fuzz run csv_parse
```

If a target finds a crashing input it is written to `fuzz/artifacts/<target>/`
and the failing bytes are printed to stderr. Reproduce locally with:

```bash
cargo +nightly fuzz run <target> fuzz/artifacts/<target>/crash-<id>
```

Please report any reproducible crash as a security advisory (see SECURITY.md)
before opening a public issue.
