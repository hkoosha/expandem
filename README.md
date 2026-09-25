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
```

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
  -h, --help                     print help.
```
