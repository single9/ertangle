# T103 — Godot Engine polyglot support across worktree provisioning, scoped diff, `ertangle check`, and `ertangle outline`

Status: done

Priority: P2　Scope: `ertangle-daemon`, `ertangle`　Depends on: T98, T100, T101, T102

## Problem

When using `ertangle` to develop **Godot Engine (Godot 4.x GDScript / C# / `.tscn` / `.gdshader`)** game projects across parallel git worktrees, agents hit four Godot-specific bottlenecks:

1. **Missing `.godot/` import and global class cache in worktrees**:
   `git worktree add` omits gitignored `.godot/`. Without `.godot/imported/` and `.godot/global_script_class_cache.cfg` / `uid_cache.bin`, running `godot --headless` in a worktree either fails with `Parse Error: Could not find type "<ClassName>" in the current scope` or wastes tens of seconds and hundreds of megabytes re-importing assets per worktree. Meanwhile, directly symlinking the entire `.godot/` directory can cause editor/shader lock contention if the user has the Godot Editor open on the main checkout.
2. **Verbose Godot diff bloat (`*.import`, `*.uid`, and inline `Packed*Array` lines in `.tscn` / `.tres`)**:
   Godot `.tscn` and `.tres` text files often embed multi-thousand-character single-line numeric arrays (`PackedByteArray(...)`, `PackedInt32Array(...)`, `PackedFloat32Array(...)`, `PackedVector2Array(...)`, `PackedVector3Array(...)`, `tile_data`) alongside `.import` / `.uid` metadata files, flooding `ertangle_diff`.
3. **Interactive window hangs and multi-line noise in Godot CLI verification (`ertangle check`)**:
   Running `godot` without `--headless` and `--quit` (or `-gexit` for GUT) hangs the agent turn indefinitely. Raw Godot output also includes engine/renderer/audio startup banners and multi-line `SCRIPT ERROR:` / `USER ERROR:` blocks with `at: ... (res://path:line)` on subsequent lines.
4. **Lack of structural navigation for `.gd`, `.tscn`, and `.gdshader` (`ertangle outline`)**:
   Reading a full 800-line `.tscn` (mostly `[sub_resource]` data) just to inspect its `[node]` hierarchy and `[connection]` signals wastes thousands of tokens.

## Evidence

- `crates/ertangle-daemon/src/git.rs:44-139` — `provision_worktree_deps` currently handles `node_modules`, `.venv`, `.env*`, and `appsettings.Development.json`, but not `.godot/`.
- `crates/ertangle-daemon/src/git.rs:216-243` — `is_generated_or_lock_file` does not include Godot `*.import`, `*.uid`, or `*.translation` files, nor does `compute_worktree_diff_with_options` fold giant `Packed*Array(...)` lines in `.tscn` / `.tres`.
- `src/cmd/check.rs:11-48` — `ProjectStack` supports `Rust`, `Node`, `DotNet`, and `Python`, but not `Godot` (`project.godot`).
- `src/cmd/outline.rs:44-60` — `Lang` supports `.rs`, `.ts`/`.js`/`.vue`, `.cs`, and `.py`, but not `.gd`, `.tscn`, or `.gdshader`.

## Analysis / decisions

- Decided for **Worktree `.godot/` provisioning (`git.rs`)**:
  - When `repo.join("project.godot").is_file()` and `repo.join(".godot").is_dir()` (and `.godot` is untracked):
    - Create a real directory `wt_dir.join(".godot")` so the worktree has its own isolated editor/shader lock space.
    - If `repo.join(".godot/imported").is_dir()`, symlink `wt_dir/.godot/imported` -> `repo/.godot/imported`.
    - Copy lightweight index files `global_script_class_cache.cfg`, `uid_cache.bin`, and `extension_list.cfg` from `repo/.godot/` into `wt_dir/.godot/` if present (so the worker starts with a valid `class_name` and UID cache, and can also safely update its own `global_script_class_cache.cfg` when adding new classes without mutating the main checkout).
    - Record `.godot` in `.ertangle/provisioned_links`, and update `cleanup_provisioned_links` so removing a provisioned `.godot` directory first unlinks `.godot/imported` (and any symlinks inside) and then removes `wt_dir/.godot` without ever touching `repo/.godot/imported`.
- Decided for **`ertangle_diff` Godot folding (`git.rs`)**:
  - Add `.import`, `.uid`, and `.translation` extensions to `is_generated_or_lock_file`.
  - When `opts.fold_generated` is true, for `.tscn` and `.tres` diff blocks, fold any diff line (`+` or `-`) exceeding 300 characters that contains `Packed` and `Array(` (e.g. `PackedInt32Array(`, `PackedByteArray(`, `PackedFloat32Array(`, `PackedVector2Array(`, `PackedVector3Array(`, `PackedColorArray(`, `PackedStringArray(`) into `<prefix>... (Godot PackedArray line folded, <N> chars; pass paths=["<path>"] to expand)`.
- Decided for **`ertangle check` Godot stack (`src/cmd/check.rs`)**:
  - Add `ProjectStack::Godot` (`"godot"`), detected when `dir.join("project.godot").is_file()`.
  - Implement `pub(crate) fn dehydrate_godot_output(raw: &str, workspace_root: &Path) -> Vec<String>`:
    - Strip ANSI sequences, Godot engine startup banners (`Godot Engine v...`, `Vulkan`, `OpenGL`, `ALSA lib`, `PulseAudio`), and blank/progress lines.
    - Combine multi-line Godot error pairs:
      `SCRIPT ERROR: <msg>` (or `ERROR: <msg>` / `USER ERROR: <msg>` / `Parse Error: <msg>`) followed by `at: <func> (res://<rel_path>:<line>)` into a single normalized line:
      `<rel_path>:<line>: <ERROR_KIND>: <msg>`
    - Convert `res://` and absolute `workspace_root` prefixes to workspace-relative paths and deduplicate identical diagnostic lines.
  - In `run_godot`:
    - Resolve Godot binary from `ERTANGLE_GODOT_BIN`, `godot`, or `godot4` (and if `gdlint` is installed, optionally run `gdlint`).
    - Run `godot --headless --path <dir> --quit` (and if `!quick` and `addons/gut/gut_cmdln.gd` exists, run `godot --headless --path <dir> -s addons/gut/gut_cmdln.gd -gexit`). Treat any `SCRIPT ERROR:` / `Parse Error:` in dehydrated output as a check failure even if Godot exits with code 0.
- Decided for **`ertangle outline` Godot support (`src/cmd/outline.rs`)**:
  - **GDScript (`.gd`)**: indentation-aware extraction of `class_name`, `extends`, `signal`, `enum`, `const`, `@export` / `@onready` variables, inner `class`, and `func` / `static func` with exact `L<start>-L<end>` ranges.
  - **Godot Scene (`.tscn`)**: section-based extraction of `[ext_resource type="Script"|"PackedScene" ...]`, `[node name="..." type="..." parent="..."]`, and `[connection signal="..." from="..." to="..." method="..."]` with exact `L<start>-L<end>` ranges (skipping noisy `[sub_resource]` blocks).
  - **Godot Shader (`.gdshader`)**: brace-aware extraction of `shader_type`, `render_mode`, `uniform`, `struct`, and shader functions (`void vertex()`, `void fragment()`, `void light()`, etc.) with exact `L<start>-L<end>` ranges.

## Implementation

1. `crates/ertangle-daemon/src/git.rs` & `crates/ertangle-daemon/src/tests.rs`:
   - Provision `.godot/` (`imported` symlink + copied `global_script_class_cache.cfg` / `uid_cache.bin` / `extension_list.cfg`) and safely clean it up in `cleanup_provisioned_links`.
   - Add `.import`, `.uid`, `.translation` to `is_generated_or_lock_file` and fold >300-char `Packed*Array(...)` lines in `.tscn` / `.tres` when `fold_generated` is true.
2. `src/cmd/check.rs`:
   - Add `ProjectStack::Godot`, `dehydrate_godot_output`, and `run_godot`.
3. `src/cmd/outline.rs`:
   - Add `Lang::Gd` (`.gd`), `Lang::Tscn` (`.tscn`), and `Lang::GdShader` (`.gdshader`) symbol extractors and unit tests.
4. `src/main.rs`, `locales/`, `README.md`, `README_EN.md`:
   - Update CLI help text and documentation to include `godot` / `.gd` / `.tscn` / `.gdshader`.

## Acceptance criteria

- Worktree creation in a Godot project (`project.godot` + `.godot/imported`) provisions `.godot/imported` via symlink and copies `global_script_class_cache.cfg`, keeps `git status` and `ertangle_diff` clean, and `remove_worktree` removes the worktree without deleting main repo `.godot/imported` assets.
- `ertangle_diff` folds `*.import` / `*.uid` files and >300-char `Packed*Array(...)` lines in `.tscn` / `.tres` by default, and expands them when listed in `paths`.
- `dehydrate_godot_output` merges `SCRIPT ERROR:` + `at: ... (res://...:line)` into `<rel_path>:<line>: SCRIPT ERROR: ...` and deduplicates repeated errors.
- `ertangle outline` extracts `L<start>-L<end>` symbols for `.gd`, `.tscn` (`[node]` / `[connection]` / `[ext_resource]`), and `.gdshader`.
- `make pre-commit` and `cargo test --workspace` pass.
