# MCP validation

Snow Shot's MCP surface targets protocol version `2026-07-28`. The public tool
catalog and Qt dispatch use `snow_shot_<domain>_<verb>` names. The private
`snow-shot-mcp/1` connection includes resource events and request cancellation
on every authenticated connection.

## Focused automated checks

Run only the affected targets after changing MCP code:

- `cargo test --manifest-path snow_shot/rust/snow-shot-mcp/Cargo.toml`
- `cargo clippy --manifest-path snow_shot/rust/snow-shot-mcp/Cargo.toml --all-targets -- -D warnings`
- `python snow_shot/tests/check_mcp_capabilities.py`
- `python snow_shot/tests/mcp_stdio_tests.py <bridge>`
- `ctest --preset test-windows-msvc-debug -R '^snow-shot-mcp-(tests|document-tests|application-tests|fixture-tests)$' --output-on-failure`

The capability check compares 101 Rust tools with Qt dispatch, the checked
catalog, and 28 screenshot input fixtures. The stdio check verifies discovery,
schemas, bounded transport, clean shutdown, and rejection of the previous
initialize protocol. The offscreen fixture uses the real bridge and Qt server to
exercise document workflows, modern resource subscriptions, Tasks, and all
28 screenshot tool contracts. Synthetic capture, provider, clipboard, and pin
ports make these checks deterministic; they do not establish native desktop
behavior.

For an interactive Windows check, run `snow_shot/tests/mcp_live_tests.py`
with an isolated test build. It captures the desktop and restores the clipboard.
The optional application fixture's `--recording` check also requires
`--platform windows` on Windows or `--platform cocoa` on macOS; it records a
native desktop region and cannot run with the offscreen Qt platform.
The performance target `snow-shot-mcp-performance-benchmark` must be built
and run with `windows-msvc-performance`.

## Remaining release validation

Native capture across monitors and scale factors, provider accounts, recording
hardware, packaged executable launch, and macOS permissions and media paths
still need platform validation before release.
