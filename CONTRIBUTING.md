# Contributing to PeaceEmu-GB

Thanks for helping improve the PeaceEmu Game Boy Advance core.

## Before changing code

- Keep the change focused on a specific hardware behavior or defect.
- Check existing code and tests before adding duplicate abstractions.
- Do not describe a partial hardware implementation as fully accurate.
- Never commit commercial ROMs, BIOS dumps, save files, credentials, or other copyrighted/proprietary assets.

## Validate changes

Run these commands from the repository root:

```sh
cargo fmt --manifest-path GameBoy/Cargo.toml -- --check
cargo check --manifest-path GameBoy/Cargo.toml --all-targets
cargo test --manifest-path GameBoy/Cargo.toml --all-targets
```

For hardware fixes, add a regression test that captures the externally observable behavior. Prefer tests for address decoding, register side effects, instruction results, timing transitions, or pixel output over tests that merely mirror implementation details.

## Pull requests

Describe:
1. The hardware behavior or bug being addressed.
2. Why the previous behavior was incorrect.
3. The implementation and any limitations that remain.
4. The commands run and their results.

Avoid combining unrelated refactors with accuracy fixes; smaller, explainable patches are easier to review.
