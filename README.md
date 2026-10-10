# Phaedrus

A clean-room implementation of Roblox Studio written in Rust, for power users who want
an editor they can read, modify and extend. Phaedrus is local-only: it opens and saves
`.rbxl`/`.rbxlx`/`.rbxm`/`.rbxmx` files and never talks to Roblox services.

It is not affiliated with Roblox Corporation.

## Quick start

```sh
rustup target add wasm32-unknown-unknown   # once: the script editor is built for wasm

cargo run --release -p phaedrus -- test_place.rbxl
```

For development, `cargo dev -- test_place.rbxl` (an alias in `.cargo/config.toml`) links
bevy dynamically, so incremental rebuilds take seconds. `cargo dev-build` builds without
running. Release builds stay statically linked.

The script editor (`crates/phaedrus_script_editor`) is built to WebAssembly by
`crates/phaedrus/build.rs` and embedded in the executable, so it needs no files or
network at runtime. If that build fails (for example without the wasm32 target, or with
`PHAEDRUS_SKIP_SCRIPT_EDITOR=1`), the reason is printed as a cargo warning and the app
uses a built-in egui editor with Luau highlighting instead. The same fallback is used if
no web view can be created (or `PHAEDRUS_NO_WEBVIEW=1` is set).

## Workspace layout

| Crate | Purpose |
|---|---|
| `crates/phaedrus_dom` | Data model on top of the rbx-dom ecosystem (`rbx_dom_weak`, `rbx_binary`, `rbx_xml`, `rbx_reflection_database`). `Document` wraps a `WeakDom` with a change journal, derived properties (`Position`, `Orientation`, `BrickColor`), reflection defaults, undo/redo (`History`) and load/save. |
| `crates/phaedrus_luau` | Luau VM (mlua, vendored Luau). Exposes `game`, `workspace`, `script`, `Instance`, `Vector3`, `Vector2`, `CFrame`, `Color3`, `BrickColor`, `UDim`, `UDim2`, `NumberRange`, `Enum`, signals (`Changed`, `ChildAdded`, `GetPropertyChangedSignal`, `RunService.Heartbeat`, ...), `task.*`, `wait`, `require` and attributes/tags, running scripts as coroutines on a frame-stepped scheduler. |
| `crates/phaedrus` | The editor: bevy viewport, egui UI, script editor host, run mode. |
| `crates/phaedrus_script_editor` | The script editor, compiled to WebAssembly: Monaco (through the `monaco` crate and wasm-bindgen) plus Luau/Roblox intellisense. A `full_moon` parse of the script feeds a small type inference for locals, functions, tables and metatable classes, `require`d ModuleScripts and callback parameters; class and enum data comes from `data/roblox-api.json` (regenerate with `scripts/gen-roblox-api.py`) and datatypes, libraries and globals from `data/luau-types.txt`. The analysis is plain Rust with native unit tests. |

### Editor modules (`crates/phaedrus/src`)

- `studio.rs`: shared editor state (document, selection, history, tools, clipboard, run state).
- `viewport/`: DOM-to-ECS sync (`sync.rs`), part meshes and materials, Studio camera,
  selection and Move/Scale/Rotate handles (`tools.rs`), lighting and graphics quality
  (`lighting.rs`), procedural sky and sun/moon (`sky.rs`), smooth terrain meshing
  (`terrain_mesh.rs`) and its LOD/streaming (`terrain.rs`).
- `ui/`: menu bar, ribbon, Explorer, Properties, Output, command bar, dialogs and theme.
- `editor/`: script editor host: a WebView2 child window over the viewport area, kept
  hidden until the editor has loaded the script, plus the instance tree snapshot
  (including ModuleScript sources) sent to it for intellisense.
- `scripting.rs`: the command-bar VM and the run VM.
- `emulators.rs`: service emulators (Test > Emulators); keeps the DataStoreService
  emulator in step with its settings and the open place, and saves its data.
- `assets.rs`: content URL resolution (see "Assets").

## Features

- **Files**: open/save binary and XML places and models (atomic writes); models open
  inside a fresh place. File > Export Selection writes `.rbxm`/`.rbxmx`.
- **Explorer**: Studio service order, class icons, filter (Ctrl+Shift+X), multi-select,
  rename (F2), drag-and-drop reparenting, context menu, double-click to open scripts.
- **Properties**: reflection-driven, grouped into Studio-like categories, filter
  (Ctrl+Shift+P), editors for bools, numbers (with arithmetic, e.g. `2*4`), strings,
  Vector3/Vector2, Color3 (picker or `[r, g, b]`), BrickColor, enums, UDim/UDim2 and
  content IDs. Edits apply to every selected instance.
- **Viewport**: parts (block, ball, cylinder, wedge, corner wedge; meshes/unions as
  bounding boxes), Roblox materials mapped to PBR, transparency, Decals and Textures
  (with stud tiling), PointLight/SpotLight/SurfaceLight.
- **Terrain**: `Terrain.SmoothGrid` is decoded and meshed as smooth (Surface Nets)
  terrain, each material coloured from `Terrain.MaterialColors`; water uses `WaterColor`
  and `WaterTransparency`. Chunks (128 studs) are meshed on background threads,
  nearest first, at a level of detail picked from camera distance: full resolution
  close up, then 2x, 4x and 8x coarser, with the full-resolution range set by the
  graphics quality level. Terrain honours the draw distance and blocks clicks on the
  parts behind it. Terrain cannot be edited yet, and materials have no textures.
- **Tools**: Select (click, Ctrl/Shift-click, Alt selects parts inside models, drag to
  move along the ground plane), Move, Scale and Rotate handles with snapping and
  world/local space.
- **Lighting and rendering**: `Lighting.Technology` maps to Voxel (soft, coarse sun
  shadows), ShadowMap (sharp cascaded sun shadows) or Future (adds local light shadows,
  SSAO and environment lighting). A graphics quality level (1-10) scales shadow
  resolution and cascades, MSAA, SSAO, bloom, environment lighting and how many local
  lights render and cast shadows (nearest to the camera first). A draw distance level
  (1-10, 250 studs to unlimited) hides parts beyond it. `TimeOfDay` and
  `GeographicLatitude` drive the sun; at night the moon lights the scene. Fog, Atmosphere
  haze, BloomEffect and ExposureCompensation are honoured. The sky is procedural (day
  gradient, sunset glow, night stars); sun and moon are billboards.
- **Scripting**: the command bar runs Luau against the open place, and its changes are
  undoable. Run (F8) executes enabled server `Script`s in Workspace and
  ServerScriptService on a copy of the place; Stop (Shift+F5) restores the edit state,
  including selection and undo history.
- **Service emulators**: the Test tab's Emulators group stands in for Roblox web
  services that need a published place. With the **DataStore** emulator on,
  `DataStoreService` (`GetDataStore` with scopes and `DataStoreOptions.AllScopes`,
  `GetGlobalDataStore`, `GetOrderedDataStore`, `ListDataStoresAsync`,
  `GetRequestBudgetForRequestType`), `DataStore` (`GetAsync`, `SetAsync`, `UpdateAsync`,
  `IncrementAsync`, `RemoveAsync`, `ListKeysAsync`, `ListVersionsAsync`,
  `GetVersionAsync`, `GetVersionAtTimeAsync`, `RemoveVersionAsync`, with
  `DataStoreKeyInfo` user IDs and metadata), `OrderedDataStore` (including
  `GetSortedAsync` pages) and the `DataStore*Options` objects work offline. Requests
  yield, values are validated and serialized like Roblox (error codes 101-105), and
  data is saved per place file in `%APPDATA%/phaedrus/datastores/<place>-<hash>.json`.
  Emulator Settings can simulate `500`/`502` errors per request type (with a chance
  and an optional data store filter) or a full outage, add latency, enforce request
  budgets and the 6-second same-key write limit, and browse or delete stored keys.
  With the emulator off, data store calls fail as in an unpublished place.
- **Script editor**: Monaco with Studio-style Luau highlighting: keywords, `local`,
  `function`, `self`, `nil` and booleans, built-ins, `:Method` calls, `.Property`
  access, function names, type annotations, strings (including backtick
  interpolation) and TODO comments each have their own colour. Lines that open a scope
  (`function ...()`, `then`, `do`, `repeat`, `else`) indent the next line, `end`/`else`
  outdent as they are typed, and Enter after an unclosed block inserts its `end` (or
  `end)` inside a call). Intellisense knows every Roblox class's
  properties, methods, events and callbacks, enums, services and datatypes
  (`Vector3`, `CFrame`, ...), and infers types through the script: locals and their
  annotations, `GetService`, `FindFirstChild`/`WaitForChild`, `Instance.new`, the live
  instance tree (`script.Parent.Config`, `workspace.Model.Door`), callback parameters
  (`part.Touched:Connect(function(hit)` gives `hit: BasePart`), loops over
  `GetChildren()`, tables and metatable-based classes, and `require`d ModuleScripts.
  It also offers hovers with types and signatures, and parameter hints.
- **Settings**: File > Phaedrus Settings has a Script Editor page (font, tab width,
  tabs or spaces, auto-indent, `end` insertion, bracket auto-closing, minimap,
  whitespace, ruler and all 42 Studio editor colours) and a Graphics page (quality
  level, draw distance, lighting technology, background FPS cap). Changes apply immediately and are
  saved to `%APPDATA%/phaedrus/settings.json` (`~/.config/phaedrus` elsewhere).

## Shortcuts

| Keys | Action |
|---|---|
| Ctrl+N / Ctrl+O / Ctrl+S / Ctrl+Shift+S | New, open, save, save as |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+1..4 | Select, Move, Scale, Rotate |
| Ctrl+L | Toggle local/world space |
| Ctrl+C / Ctrl+X / Ctrl+V / Ctrl+Shift+V | Copy, cut, paste, paste into |
| Ctrl+D / Del | Duplicate / delete |
| Ctrl+G / Ctrl+U | Group / ungroup |
| F2 | Rename |
| F | Focus the selection |
| F8 / Shift+F5 | Run / stop |
| Ctrl+W | Close the script tab |
| RMB drag / MMB drag / wheel | Look / pan / zoom |
| WASD, Q/E (Shift = slow) | Fly the camera |

## Memory

Memory use was a design goal:

- Properties equal to their class default are dropped on load
  (`phaedrus_dom::document::compact_defaults`). Reads fall back to reflection defaults
  and the serializers write defaults back, so this is invisible to users. On a synthetic
  20,000-part place the DOM shrinks from 214 MB to 32 MB
  (`cargo run --release -p phaedrus_dom --example memory -- test_place.rbxl 20000`).
- All parts of a shape share one GPU mesh (scaled per instance); materials are shared by
  quantized appearance; generated meshes keep no CPU copy.
- Undo history is bounded (256 steps); removed instances are kept detached only while
  history can still restore them.
- Output is capped at 5,000 lines. The script editor's web view is created only when a
  script is opened and is destroyed when the last script tab closes.
- The 3D view renders only while visible, and the app throttles to a low frame rate
  when unfocused (configurable in File > Phaedrus Settings > Graphics).
- Large places: parts are frustum-culled on the GPU (bevy's CPU culling re-tests and
  re-extracts every part as the camera turns), draw distance culling works on 128-stud
  grid cells, tiny parts cast no sun shadow, shadowed local lights are capped and the
  sky's environment map is filtered once rather than every frame.

A small place uses about 520 MB of working set on Windows with Vulkan, almost all of it
the bevy/wgpu/driver baseline (DX12 is higher on NVIDIA).

## Assets

Phaedrus never downloads Roblox assets. Content URLs resolve as follows:

1. `rbxasset://...` maps into a locally installed Roblox Studio `content` folder if one
   exists (the sun and moon textures come from there; procedural ones are used otherwise).
2. `rbxassetid://ID` maps to `%APPDATA%/phaedrus/assets/ID.png|jpg|dds` if you place a
   file there.
3. Otherwise a placeholder is used. Unresolved `Texture`s use a procedural grid, which
   matches the default baseplate.

## Testing

```sh
cargo test --workspace
```

The DOM tests load `test_place.rbxl` and check shapes, derived properties, undo/redo and
binary/XML round-trips. The VM tests run Luau against the place (property writes such as
`workspace.Cube.Position = Vector3.new(10, 0, 0)`, datatypes, events, `task.wait`
scheduling and error reporting). The DataStore emulator tests cover standard and ordered
stores, persistence across emulator instances, simulated failures and request budgets.

For visual checks, `PHAEDRUS_SCREENSHOT=out.png` saves a screenshot after startup and
exits. Optional variables: `PHAEDRUS_EXEC` (command-bar code), `PHAEDRUS_SELECT`
(comma-separated Workspace child names), `PHAEDRUS_TOOL` (`move`/`scale`/`rotate`),
`PHAEDRUS_RUN=1`, `PHAEDRUS_OPEN_SCRIPT=1`, `PHAEDRUS_OPEN_EMULATORS` (`settings`/`data`;
also turns the DataStore emulator on), `PHAEDRUS_QUALITY` (1-10), `PHAEDRUS_TECH`
(`voxel`/`shadowmap`/`future`) and `PHAEDRUS_SCREENSHOT_FRAME`.

For performance, `PHAEDRUS_BENCH=600 phaedrus place.rbxl` waits for the place to load,
places the camera over the densest area, turns it a full circle over 600 frames with
vsync off and prints average and 1%-low frame times. It honours `PHAEDRUS_QUALITY`,
`PHAEDRUS_TECH` and `PHAEDRUS_DRAW_DISTANCE`; `PHAEDRUS_BENCH_CAM=x,y,z` sets the camera,
`PHAEDRUS_BENCH_EXEC` runs command-bar code after loading, `PHAEDRUS_BENCH_SHOT=out.png`
saves a screenshot and `PHAEDRUS_BENCH_PASSES=1` prints per-pass GPU/CPU timings.
Benchmark and screenshot runs never save settings.

## Known gaps

- Play (F5) with a character and client/server split is not implemented; F5 falls back
  to Run. LocalScripts do not run.
- No physics: unanchored parts stay where they are.
- Terrain editing, MeshPart/Union geometry, SurfaceGui/BillboardGui rendering and custom skybox
  textures (unless available locally) are not rendered.
- The Luau API covers common instance, datatype, signal and task functionality, not the
  full Roblox API. Services such as TweenService and HttpService are not implemented;
  DataStoreService is available through its emulator. The deprecated
  `GlobalDataStore:OnUpdate` is not supported.
- Native web view overlays: egui popups cannot draw on top of the Monaco editor area.
- Output timestamps are UTC.

## Roadmap

- Play-testing with a client/server model, character controller and basic physics.
- Luau plugins and widget windows.
- Terrain tools, mesh rendering, more of the Roblox API.
