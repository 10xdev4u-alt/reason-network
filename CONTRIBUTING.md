# Contributing

Thanks for helping. tincan tries to stay small and to just work: a change that removes code or a step is worth more
than a new option. For anything bigger than a fix, open an [issue](https://github.com/larstalian/tincan/issues) or a
[discussion](https://github.com/larstalian/tincan/discussions) first so we can agree on the shape.
[Good first issues](https://github.com/larstalian/tincan/labels/good%20first%20issue) are a place to start, and
[DESIGN.md](DESIGN.md) explains how the pieces fit.

## Build and test

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
cargo build && python3 tests/e2e/two_machines.py   # two machines over iroh (needs internet)
python3 tests/e2e/hooks.py && python3 tests/e2e/guided.py
```

The e2e tests start their own daemons with their own `TINCAN_HOME`, so they leave an installed tincan alone. Run
them from a plain terminal: inside a Claude Code session, two_machines.py also delivers its test messages into that
session.

## Pull requests

- One change per pull request, saying what it changes and why. CI runs fmt, clippy and the tests on Linux, macOS and
  Windows.
- Match the code around you: plain names, few comments, and comments that say why.
- Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you,
  as defined in the Apache-2.0 license, shall be dual licensed as MIT OR Apache-2.0, without any additional terms or
  conditions.

Security issues go through private reporting instead: see [SECURITY.md](SECURITY.md).

## Releasing

Push a `v*` tag. A tag with a dash (`v1.2.0-rc.1`) makes a test release that installs and auto-updates never pick.
