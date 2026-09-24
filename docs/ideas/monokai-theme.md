# Third built-in theme: Monokai

`docs/blueprint.md` originally listed three built-in `syntect` themes for
Phase 7: dark, light, and monokai. Phase 7 ships only dark
(`base16-ocean.dark`) and light (`base16-ocean.light`), both already present
in `syntect`'s bundled `ThemeSet::load_defaults()` set — no bundled default
theme is named "monokai".

Adding a Monokai theme requires embedding a separately-sourced theme asset
(`.tmTheme` or a `syntect` binary dump — the `dump-load` `syntect` feature is
already enabled in `Cargo.toml`, anticipating this) rather than selecting an
existing bundled name. Before implementing:

- Identify a concrete Monokai `.tmTheme` source with a license compatible
  with this project's Apache-2.0 licensing (many popular Monokai theme files
  circulating online are MIT-licensed derivatives of Wimer Hazenberg's
  original Monokai; the exact source and its license header must be checked
  and the attribution preserved).
- Decide whether to embed the raw `.tmTheme` XML (parsed at startup via
  `ThemeSet::load_from_reader`) or pre-convert it to a `syntect` binary dump
  at build time (`dump-load` feature) for faster startup.
- Wire it into `src/highlight.rs`'s theme selection and the `theme`
  config key.

Not scheduled to a phase yet — pick this up when a third theme is actually
wanted, or fold it into whichever future phase does further rendering polish.
