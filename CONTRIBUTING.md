# Contributing to Rust Browser

Thanks for helping! Rust Browser aims to be the lightest, fastest browser on Windows, so
changes that keep memory and CPU use low are especially welcome.

## Reporting a bug or idea

Open an [issue](https://github.com/she1bys/rust-browser/issues) with:

- what you did, what you expected, and what happened instead
- your Windows version and Rust Browser version (Settings > About)
- a screenshot if it's about how something looks

## Sending a change

1. Fork the repository on GitHub and clone your fork.
2. Make your change. See "Change it yourself" in the [README](README.md) to set up and run it.
3. Before you commit, run the same checks as CI:

   ```bash
   cargo fmt --all
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

   For changes to the browser's behaviour or UI, also run the click-through test
   (see "Testing" in the README):

   ```bash
   cargo build --release
   powershell -ExecutionPolicy Bypass -File tests\ui-smoke.ps1
   ```

4. Push to your fork and open a pull request describing what changed and why.

## Guidelines

- Keep it light: avoid new dependencies unless they're small and clearly worth it, and
  measure memory before and after for anything that runs all the time.
- Web pages are untrusted: they must never get a message channel to the browser.
- Don't add Chromium command-line flags that turn off safety or power saving (for example
  frame-rate limits, SmartScreen, or occlusion); a test in `src/display.rs` guards this.
- Add a test for logic that can be tested without a window (settings, URL handling, IPC
  messages, memory accounting).

By contributing, you agree that your contribution is licensed under the [MIT License](LICENSE).
