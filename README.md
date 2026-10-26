# Expandem

A command-line tool to expand selected Rust macros. It uses rust-analyzer behind
the scene.

## Usage

```sh
# Install from crates.io
cargo install exapndem

# Expand all non-std and non-core macros
expandem path/to/source.rs

# Expand only selected macros
expandem path/to/source.rs my_crate::some_macro

# Expand std macros too
expandem path/to/source.rs --include-std

# Simplify expanded branches, parentheses, and path prefixes
expandem path/to/source.rs --fix
```

`--fix` simplifies the expanded output without adding imports:

- Removes literal `if true` conditions and their unreachable `else` branches,
  including in `return` expressions and generated implementations. The selected
  block keeps its scope and attributes; other conditions are unchanged.
- Removes redundant expression and type parentheses while preserving precedence,
  required grouping, attributes, comments, and token boundaries.
- Shortens path prefixes only when the shorter path resolves to the same item in
  the expanded file's original workspace. For example,
  `::core::marker::PhantomData` can become `core::marker::PhantomData`, and
  `::core::result::Result` can become `Result` when unshadowed. Required
  qualification, generic arguments, and unresolved prefixes are retained.
  Imports are not shortened to their own bindings.

Unexpanded macro token trees are left untouched.

Usage:

```shell
$ expandem --help
expand selected Rust macros in a source file

usage:
 expandem [OPTIONS] <SOURCE> [MACROS]...

arguments:
  <SOURCE>     source file to transform.
  [MACROS]...  macro paths to expand; do not include `!`. omit to expand all macros.

options:
      --include-std              expand macros defined by the `std` crate.
      --include-core             expand macros defined by the `core` crate.
  -n, --negate                   treat MACROS as exclusions rather than inclusions.
      --skip-build-scripts       skip running Cargo for discovering build-script output and proc macros.
      --skip-proc-macros         do not start the proc-macro server or expand procedural macros.
      --proc-macro-srv <PATH>    path to rust-analyzer's proc-macro server.
      --fix                      simplify branches, parentheses, and paths.
  -h, --help                     print help.
```

TODO:

- Currently if a procedural macro is consumed by another one, the later ones are
  not dropped from the output as they should be.
