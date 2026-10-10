# AGENTS.md

Instructions for AI coding agents (Claude Code, Codex and others) working in this
repository. This is the single source of truth: `CLAUDE.md` and `CODEX.md` only point
here, so edit this file rather than those.

## Project

Phaedrus is a clean-room implementation of Roblox Studio in Rust (edition 2024): a bevy
viewport, an egui UI, a Luau VM and a Monaco script editor compiled to WebAssembly.
`README.md` is the user-facing reference for features, shortcuts, environment variables
and known gaps; read it before working on an unfamiliar area.

## Ground rules

- **Local only.** Phaedrus never talks to Roblox services and never downloads assets.
  Do not add network calls at runtime. Web services are stood in for by emulators
  (`crates/phaedrus/src/emulators.rs`, `crates/phaedrus_luau/src/datastore.rs`).
- **Clean room.** Do not copy code, shaders, textures or other assets from Roblox
  Studio or the Roblox client. Behaviour is matched from public documentation and
  observation. `rbxasset://` content is only ever read from a locally installed Studio.
- **Not affiliated with Roblox.** Do not add Roblox branding or imply endorsement.
- Behaviour should match Roblox Studio where a feature exists in both (error codes,
  defaults, property semantics, service order). When unsure what Studio does, say so
  rather than guessing.

## Workspace

| Crate | What lives there |
|---|---|
| `crates/phaedrus_dom` | Data model over rbx-dom: `Document`, change journal, derived properties, reflection defaults, undo/redo (`History`), load/save. |
| `crates/phaedrus_luau` | Luau VM (mlua, vendored), Roblox datatypes, instances, signals, `task` scheduler, DataStore emulator. |
| `crates/phaedrus` | The editor app: `studio.rs` (shared state), `viewport/`, `ui/`, `editor/` (WebView2 host for the script editor), `scripting.rs`, `emulators.rs`, `assets.rs`. |
| `crates/phaedrus_script_editor` | Monaco + Luau/Roblox intellisense, built for wasm32. Analysis is plain Rust and also builds natively for tests. |

## Commands

```sh
rustup target add wasm32-unknown-unknown       # once, for the script editor

cargo dev -- test_place.rbxl                   # run with bevy dynamically linked (fast rebuilds)
cargo dev-build                                # build only
cargo run --release -p phaedrus -- test_place.rbxl
cargo test --workspace                         # all tests
cargo test -p phaedrus_dom                     # one crate
```

- Prefer `cargo dev-build` / `cargo test -p <crate>` while iterating; full release
  builds are slow.
- Set `PHAEDRUS_SKIP_SCRIPT_EDITOR=1` to skip the wasm build when you are not touching
  the script editor. The app then uses its egui fallback editor.
- Windows is the primary platform. `.cargo/config.toml` links with `rust-lld.exe`.

## Build invariants

- `crates/phaedrus/build.rs` builds `phaedrus_script_editor` for wasm32, runs
  wasm-bindgen and embeds the bundle. A failure there must stay a cargo warning plus
  the fallback editor, never a hard build error.
- `wasm-bindgen` (in `phaedrus_script_editor`) and `wasm-bindgen-cli-support` (in
  `phaedrus`) are pinned to the same exact version. Change both together.
- The `dynamic_linking` feature is dev-only. Never enable it for release builds.
- `crates/phaedrus_script_editor/data/roblox-api.json` is generated. Regenerate it with
  `python scripts/gen-roblox-api.py` instead of editing it by hand.
  `data/luau-types.txt` is hand-maintained.
- `serde_json`'s `preserve_order` feature in the script editor is required (Monarch
  grammars depend on key order).

## Code conventions

- Match the surrounding code: naming, comment density and idiom. There is no
  `rustfmt.toml` and lines often run past 100 columns, so do not run `cargo fmt` over
  whole files or reformat code you are not changing.
- Use `anyhow` for errors in application and loader code; do not `unwrap()` on user
  data (place files, scripts, settings).
- Avoid `unsafe`; the codebase has almost none.
- Unit tests sit in `#[cfg(test)]` modules next to the code; integration tests are in
  `crates/*/tests/`. DOM and VM tests load `test_place.rbxl`.
- Editor changes to the place go through `Document`/`History` so they are undoable,
  including changes made from the command bar.

## Memory and performance

Memory use is a design goal (see "Memory" in `README.md`). Keep to it:

- Do not store properties equal to their class default; read through reflection
  defaults (`phaedrus_dom::document::compact_defaults`).
- Share meshes and materials instead of creating per-part copies; generated meshes keep
  no CPU-side copy.
- Keep caches and histories bounded (undo is 256 steps, Output is 5,000 lines).
- Heavy work such as terrain meshing runs on background threads, not the frame.

## Verifying changes

1. `cargo test --workspace` (or the affected crate) must pass. Add tests for new DOM,
   VM, emulator or analysis behaviour.
2. For visual or UI changes, use the screenshot mode:
   `PHAEDRUS_SCREENSHOT=out.png cargo dev -- test_place.rbxl`, plus `PHAEDRUS_EXEC`,
   `PHAEDRUS_SELECT`, `PHAEDRUS_TOOL`, `PHAEDRUS_RUN`, `PHAEDRUS_OPEN_SCRIPT`,
   `PHAEDRUS_QUALITY`, `PHAEDRUS_TECH` as needed (see "Testing" in `README.md`). Look
   at the image before saying a visual change works.
3. For rendering or large-place performance work, measure with `PHAEDRUS_BENCH` before
   and after, and report the numbers.
4. If you could not run a step (no GPU, no wasm target, no WebView2), say so.

## Documentation

`README.md` documents user-visible behaviour. When you add or change a feature,
shortcut, setting, environment variable or limitation, update the matching README
section (including "Known gaps") in the same change.

## Git

- Commit only when asked. Keep commits focused, with an imperative summary line.
- Never commit `target/`, `PROMPT.md`, or large local place files such as
  `Large_Place_File.rbxl`. `test_place.rbxl` is the only tracked fixture.
- Do not rewrite published history or force-push without explicit approval.
