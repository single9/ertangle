# T78 — Command subcommand autocomplete, local project folder picker, Space P shortcut

Priority: P2    Scope: `ertangle-tui`    Depends on: T59

## Problem

1. Command-line autocomplete only completes the top-level verb. After typing a space,
   `:project`, `:scope`, `:layout`, `:notify`, `:web`, `:ended`, `:queue`, and
   `:task-status` show nothing useful, and Tab does not complete their subcommands or
   flags. Only `:model`, `:effort`, `:skill`, and `:set` had argument candidates.
2. The project picker (`T59`) lists known roots and accepts a typed path, but there is
   no way to browse the local filesystem. Users must know the path beforehand.
3. There is no direct keyboard shortcut for opening the project picker.

## Decisions

- Add a single `SUBCOMMANDS` table in `commands.rs`, shared by the hint popup and Tab
  completion, so every enumerated subcommand/flag autocompletes consistently.
- Reuse `attach::complete_path` for path arguments (`:project open <path>`, `:export`)
  so filesystem paths autocomplete too. Fix the common-prefix computation to respect
  UTF-8 character boundaries (needed for Chinese folder names).
- Add a `PickStage::Directory` stage to the existing picker: an in-TUI directory browser
  (no new native file-dialog dependency). It lists subdirectories of the current
  folder plus `../` and a `use current folder` row. `Enter` descends or selects,
  `Backspace` goes up, `Esc` returns to the project list.
- Add `Space P` (and `Space p`) to the root which-key menu, wired to `:project open`.

## Implementation

- `crates/ertangle-tui/src/commands.rs`: `SUBCOMMANDS`, `subcommand_candidates`,
  `path_candidates`; extended `arg_candidates`; third `:skill` argument.
- `crates/ertangle-tui/src/input.rs`: safe common prefix in `complete_arg`.
- `crates/ertangle-tui/src/menu.rs`: root menu items, `PickStage::Directory`,
  `directory_items`, `start_directory_picker`, browse entry in `start_project_picker`,
  and key handling in `handle_pick_key`.
- `crates/ertangle-tui/src/help.rs`: document the new shortcut and autocomplete.
- Locales: `make i18n-extract` + English translations in `locales/en.po`.

## Acceptance criteria

- `:project op<Tab>` completes to `:project open `; `:scope a<Tab>` completes to `all`.
- `:project open /ho<Tab>` completes filesystem paths.
- `:project open` opens the project picker; its `browse` entry opens the directory
  browser; selecting a folder switches the active project.
- `Space P` and `Space p` open the project picker.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `make i18n-check`,
  and `cargo test -p ertangle-tui` pass.
