#![allow(clippy::needless_return)]

pub use failure::ExpandemError;

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Source file to transform.
    pub source: std::path::PathBuf,

    /// Path to rust-analyzer's proc-macro server
    pub proc_macro_srv: Option<std::path::PathBuf>,

    /// Source paths of macros to include or exclude. An empty set selects every macro.
    pub macros: std::collections::BTreeSet<String>,

    /// Expand macros defined by the `std` crate. Disabled by default.
    pub include_std: bool,

    /// Expand macros defined by the `core` crate. Disabled by default.
    pub include_core: bool,

    /// Treat [`Self::macro_paths`] as an exclusion list rather than an inclusion list.
    /// Does not affect std and core flags.
    pub negate: bool,

    /// Run Cargo to collect build-script outputs and proc-macro dylibs.
    pub skip_build_scripts: bool,

    /// Skip starting a proc-macro server and loading procedural macros.
    pub skip_proc_macros: bool,
}

pub fn expand(it: Options) -> Result<String, ExpandemError> {
    return internal::expand(it);
}

pub fn parse_options(
    bin_name: Option<String>,
    args: impl Iterator<Item = String>,
) -> Result<Options, (String, i32)> {
    return args::parse(bin_name, args);
}

mod internal {
    use super::*;
    use std::fmt::Display;

    use ast::MacroCall;
    use log::{
        trace,
        warn,
    };
    use ra_ap_hir::{
        Crate,
        Macro,
    };
    use ra_ap_ide::{
        RootDatabase,
        Semantics,
        TextRange,
    };
    use ra_ap_ide_db::base_db::{
        CrateOrigin,
        LangCrateOrigin,
        SourceDatabase,
    };
    use ra_ap_ide_db::syntax_helpers::prettify_macro_expansion;
    use ra_ap_load_cargo::{
        LoadCargoConfig,
        ProcMacroServerChoice,
        load_workspace_at,
    };
    use ra_ap_project_model::{
        CargoConfig,
        RustLibSource,
    };
    use ra_ap_syntax::{
        AstNode,
        NodeOrToken,
        SourceFile,
        SyntaxNode,
        ast::{
            self,
            HasAttrs,
        },
    };
    use ra_ap_vfs::{
        AbsPathBuf,
        FileId,
        Vfs,
        VfsPath,
    };
    use std::fs;
    use std::ops::Range;

    fn sub(it: impl Display) -> String {
        let max = 30;

        let it = it.to_string();
        return if it.len() <= max {
            it
        }
        else {
            format!("{}...", &it[0..max])
        };
    }

    pub(crate) fn expand(options: Options) -> Result<String, ExpandemError> {
        trace!("creating workspace");
        let workspace = Workspace::load(options)?;

        trace!("creating editor");
        let editor = workspace.editor();

        trace!("opening context");
        let ctx = Context::new(&workspace, editor)?;

        trace!("ekran...");
        return ctx.ekran();
    }

    #[derive(Debug, Clone, Copy)]
    enum MacroOrigin {
        Core,
        Std,
        Other,
    }

    struct Workspace {
        db: RootDatabase,
        source: AbsPathBuf,
        vfs: Vfs,
        options: Options,
    }

    struct Context<'db> {
        editor: Editor<'db>,
        file_id: FileId,
        krate: Crate,
    }

    impl<'db> Context<'db> {
        fn new(
            workspace: &Workspace,
            editor: Editor<'db>,
        ) -> Result<Self, ExpandemError> {
            let (file_id, _) = editor
                .vfs
                .file_id(&VfsPath::from(workspace.source.clone()))
                .ok_or("not a source file owned by its discovered workspace")?;
            let krate = editor.sema.first_crate(file_id).ok_or_else(|| {
                ExpandemError::of_str("the source file belongs to no crate")
            })?;

            return Ok(Context {
                editor,
                file_id,
                krate,
            });
        }

        fn ekran(&self) -> Result<String, ExpandemError> {
            let root = self.root();
            let syntax = root.syntax();

            let attribute_edits = self.selected_attribute_edits(syntax)?;

            let ranges = attribute_edits
                .iter()
                .map(|edit| edit.range)
                .collect::<Vec<_>>();

            let derive_edits = self.editor.selected_derive_edits(
                &self.krate,
                syntax,
                &ranges,
            )?;

            let function_edits = self.editor.outermost_selected_calls_edits(
                &self.krate,
                syntax,
                syntax,
                ranges,
            )?;

            let mut edits = attribute_edits;
            edits.extend(derive_edits);
            edits.extend(function_edits);
            edits.sort_unstable_by_key(|edit| {
                std::cmp::Reverse((edit.range.start(), edit.range.end()))
            });
            if !is_disjoint(&edits) {
                return ExpandemError::fail(
                    "selected macro expansions overlap",
                );
            }

            let mut output = self.output();
            for it in edits {
                trace!("{:?}", it);
                output.replace_range(it.to_range(), &it.replacement);
            }

            return Ok(output);
        }

        fn output(&self) -> String {
            return self
                .editor
                .sema
                .db
                .file_text(self.file_id)
                .text(self.editor.sema.db)
                .to_string();
        }

        fn root(&self) -> SourceFile {
            trace!("reading file: {:?}", self.file_id);
            return self.editor.sema.parse_guess_edition(self.file_id);
        }

        fn selected_attribute_edits(
            &self,
            root: &SyntaxNode,
        ) -> Result<Vec<Edit>, ExpandemError> {
            trace!("finding edit candidates");
            let mut candidates = root
                .descendants()
                .filter_map(ast::Item::cast)
                .inspect(|it| trace!("trying to expand: {}", sub(it)))
                .filter_map(|it| {
                    let expansion =
                        match self.editor.sema.expand_attr_macro(&it) {
                            None => {
                                trace!("did not expand: {}", sub(&it));
                                return None;
                            }
                            Some(it) => it,
                        };
                    if let Some(err) = expansion.err {
                        warn!("expansion error: {:?}", err);
                    }
                    return Some((
                        it.syntax().text_range(),
                        expansion.value.value,
                    ));
                })
                .collect::<Vec<_>>();

            candidates.sort_unstable_by_key(|(range, _)| {
                (range.start(), std::cmp::Reverse(range.end()))
            });

            trace!("candidates: {:?}", candidates);

            let mut edits = Vec::new();
            for (range, expansion) in candidates {
                if !edits.iter().any(|it: &Edit| covers(it.range, range)) {
                    let edit = Edit {
                        range,
                        replacement: self
                            .editor
                            .expand_selected_node(&self.krate, &expansion)?,
                    };
                    edits.push(edit);
                }
            }

            trace!("edits: {:?}", edits);

            Ok(edits)
        }
    }

    impl Workspace {
        fn load(options: Options) -> Result<Self, ExpandemError> {
            let source = AbsPathBuf::assert_utf8(
                fs::canonicalize(&options.source)
                    .map_err(ExpandemError::wrap)?,
            );
            trace!("reading workspace: {source}");

            let cargo_config = CargoConfig {
                sysroot: Some(RustLibSource::Discover),
                all_targets: true,
                ..Default::default()
            };

            let load_config = LoadCargoConfig {
                load_out_dirs_from_check: !options.skip_build_scripts,
                prefill_caches: false,
                num_worker_threads: 1,
                proc_macro_processes: 1,
                with_proc_macro_server: if !options.skip_proc_macros {
                    match options.proc_macro_srv.as_deref() {
                        Some(path) => ProcMacroServerChoice::Explicit(
                            AbsPathBuf::assert_utf8(
                                fs::canonicalize(path)
                                    .map_err(ExpandemError::wrap)?,
                            ),
                        ),
                        None => ProcMacroServerChoice::Sysroot,
                    }
                }
                else {
                    ProcMacroServerChoice::None
                },
            };

            let (db, vfs, _proc_macro) = load_workspace_at(
                source.as_ref(),
                &cargo_config,
                &load_config,
                &|_| {},
            )
            .map_err(|it| ExpandemError::Other(it.into_boxed_dyn_error()))?;

            trace!("workspace loaded");
            return Ok(Workspace {
                db,
                source,
                vfs,
                options,
            });
        }

        fn editor(&self) -> Editor<'_> {
            return Editor {
                sema: Semantics::new(&self.db),
                options: &self.options,
                vfs: &self.vfs,
            };
        }
    }

    struct Editor<'a> {
        sema: Semantics<'a, RootDatabase>,
        options: &'a Options,
        vfs: &'a Vfs,
    }

    //noinspection DuplicatedCode
    impl Editor<'_> {
        fn expand_selected_node(
            &self,
            krate: &Crate,
            expanded: &SyntaxNode,
        ) -> Result<String, ExpandemError> {
            trace!("loading HIR: {expanded}");

            let output = self
                .sema
                .hir_file_for(expanded)
                .macro_file()
                .map(|macro_file| {
                    trace!("prettify_macro_expansion: {expanded}");
                    prettify_macro_expansion(
                        self.sema.db,
                        expanded.clone(),
                        macro_file.expansion_span_map(self.sema.db),
                        (*krate).into(),
                    )
                })
                .unwrap_or_else(|| expanded.clone());

            let mut edits = self.outermost_selected_calls_edits(
                krate,
                expanded,
                &output,
                vec![],
            )?;

            edits.sort_unstable_by_key(|edit| {
                std::cmp::Reverse((edit.range.start(), edit.range.end()))
            });
            if !is_disjoint(&edits) {
                return ExpandemError::fail(
                    "selected macro expansions overlap",
                );
            }

            let mut output = output.text().to_string();
            for replacement in edits {
                output.replace_range(
                    usize::from(replacement.range.start())
                        ..usize::from(replacement.range.end()),
                    &replacement.replacement,
                );
            }

            return Ok(output);
        }

        fn selected_derive_edits(
            &self,
            krate: &Crate,
            root: &SyntaxNode,
            attribute_ranges: &[TextRange],
        ) -> Result<Vec<Edit>, ExpandemError> {
            trace!("derive edits...");

            let mut edits = Vec::new();

            for item in root.descendants().filter_map(ast::Item::cast) {
                let item_range = item.syntax().text_range();

                if attribute_ranges
                    .iter()
                    .any(|range| covers(*range, item_range))
                {
                    continue;
                }

                let mut generated = Vec::new();

                item
                    .attrs()
                    .filter_map(|it| {
                        it.meta().map(|meta| (meta, it.syntax().text_range()))
                    })
                    .filter(|(meta, _range)| {
                        meta.path().is_none_or(|p| p.to_string() != "derive")
                    })
                    .map(|(meta, range)| (derive_entries(&meta), meta, range))
                    .map(|(entries, meta, range)| {
                        (
                            self.sema
                                .resolve_derive_macro(&meta)
                                .unwrap_or_default()
                                .into_iter()
                                .flatten()
                                .collect::<Vec<_>>(),
                            entries,
                            meta,
                            range,
                        )
                    })
                    .map(|(resolved, entries, meta, range)| {
                        (
                            entries
                                .iter()
                                .enumerate()
                                .filter(|(index, entry)| {
                                    let origin = resolved
                                        .get(*index)
                                        .map(|it| self.origin(*it))
                                        .unwrap_or(MacroOrigin::Other);
                                    self.selects(entry, origin)
                                })
                                .map(|(index, _)| index)
                                .collect::<Vec<_>>(),
                            resolved,
                            entries,
                            meta,
                            range,
                        )
                    })
                    .filter(|(selected, _, _, _, _)| selected.is_empty())
                    .map(|(selected, resolved, entries, meta, range)| {
                        (
                            entries
                                .iter()
                                .enumerate()
                                .filter(|(index, _)| !selected.contains(index))
                                .map(|(_, entry)| entry.clone())
                                .collect::<Vec<_>>(),
                            selected,
                            resolved,
                            entries,
                            meta,
                            range,
                        )
                    })
                    .try_for_each(
                        |(
                             remaining,
                             selected,
                             _resolved,
                             _entries,
                             meta,
                             range,
                         )| {
                            let expansions = self
                                .sema
                                .expand_derive_macro(&meta)
                                .ok_or_else(|| ExpandemError::of_str("could not expand macro"))?;
                            if expansions.len() != 1 {
                                return ExpandemError::fail(
                                    "could not match derive macros to their expansions",
                                );
                            }

                            for index in &selected {
                                let Some(Some(expansion)) = expansions.get(*index)
                                else {
                                    return ExpandemError::fail("derive macro has no expansion");
                                };

                                generated.push(self.expand_selected_node(
                                    krate,
                                    &expansion.value,
                                )?);
                            }

                            let replacement = if remaining.is_empty() {
                                String::new()
                            } else {
                                format!("#[derive({})]", remaining.join(", "))
                            };

                            edits.push(Edit {
                                range,
                                replacement,
                            });

                            Ok(())
                        },
                    )?;

                if !generated.is_empty() {
                    edits.push(Edit {
                        range: TextRange::empty(item_range.end()),
                        replacement: format!("\n{}", generated.join("\n")),
                    });
                }
            }

            Ok(edits)
        }

        fn outermost_selected_calls_edits(
            &self,
            krate: &Crate,
            semantic_node: &SyntaxNode,
            output_node: &SyntaxNode,
            attribute_ranges: Vec<TextRange>,
        ) -> Result<Vec<Edit>, ExpandemError> {
            trace!(
                "outermost_selected_calls_edits: crate={:?}, node={:?}, attribute={:?}",
                krate, semantic_node, attribute_ranges,
            );

            let mut semantic_calls =
                semantic_node.descendants().filter_map(MacroCall::cast);

            let mut output_calls =
                output_node.descendants().filter_map(MacroCall::cast);

            let mut edits = Vec::new();

            loop {
                let (semantic_call, output_call) = match (
                    semantic_calls.next(),
                    output_calls.next(),
                ) {
                    (None, None) => return Ok(edits),
                    (Some(semantic_call), Some(output_call)) => {
                        (semantic_call, output_call)
                    }
                    _ => {
                        return ExpandemError::fail(
                            "macro expansion prettification changed macro call structure",
                        );
                    }
                };

                if !self.is_selected_call(&semantic_call)
                    || semantic_call
                        .syntax()
                        .ancestors()
                        .skip(1)
                        .filter_map(MacroCall::cast)
                        .any(|ancestor| self.is_selected_call(&ancestor))
                    || attribute_ranges.iter().any(|range| {
                        covers(*range, semantic_call.syntax().text_range())
                    })
                {
                    trace!("skip: {semantic_call:?}");
                    continue;
                }

                edits.push(Edit {
                    range: output_call.syntax().text_range(),
                    replacement: self.expand_selected_node(
                        krate,
                        &self
                            .sema
                            .expand_macro_call(&semantic_call)
                            .ok_or_else(|| {
                                ExpandemError::of_str(
                                    "could not expand macro call",
                                )
                            })?
                            .value,
                    )?,
                });
            }
        }

        fn is_selected_call(
            &self,
            call: &MacroCall,
        ) -> bool {
            return call
                .path()
                .map(|it| it.to_string())
                .map(|it| {
                    let origin = self
                        .sema
                        .resolve_macro_call(call)
                        .map(|mac| self.origin(mac))
                        .unwrap_or(MacroOrigin::Other);
                    self.selects(it.as_str(), origin)
                })
                .unwrap_or(false);
        }

        fn origin(
            &self,
            it: Macro,
        ) -> MacroOrigin {
            return match it
                .module(self.sema.db)
                .krate(self.sema.db)
                .origin(self.sema.db)
            {
                CrateOrigin::Lang(LangCrateOrigin::Core) => MacroOrigin::Core,
                CrateOrigin::Lang(LangCrateOrigin::Std) => MacroOrigin::Std,
                _ => MacroOrigin::Other,
            };
        }

        fn selects(
            &self,
            path: &str,
            origin: MacroOrigin,
        ) -> bool {
            return match origin {
                MacroOrigin::Std => self.options.include_std,
                MacroOrigin::Core => self.options.include_core,
                _ => {
                    let mut cond = self.options.macros.is_empty()
                        || self.options.macros.contains(path);
                    if self.options.negate {
                        cond = !cond;
                    }
                    cond
                }
            };
        }
    }

    #[derive(Debug)]
    struct Edit {
        range: TextRange,
        replacement: String,
    }

    impl Edit {
        fn to_range(&self) -> Range<usize> {
            return usize::from(self.range.start())
                ..usize::from(self.range.end());
        }
    }

    fn derive_entries(meta: &ast::Meta) -> Option<String> {
        if let ast::Meta::TokenTreeMeta(meta) = meta
            && let Some(token_tree) = meta.token_tree()
        {
            let name = token_tree
                .token_trees_and_tokens()
                .flat_map(|it| match it {
                    NodeOrToken::Token(token) => Some(token),
                    _ => None,
                })
                .flat_map(|it| match it.text() {
                    "," => Some(",".to_string()),
                    "(" | ")" => None,
                    txt if txt.trim().is_empty() => None,
                    txt => Some(txt.to_string()),
                })
                .collect::<String>();

            if !name.is_empty() {
                return Some(name);
            }
        }

        return None;
    }

    fn covers(
        outer: TextRange,
        inner: TextRange,
    ) -> bool {
        outer.start() <= inner.start() && inner.end() <= outer.end()
    }

    fn is_disjoint(edits: &[Edit]) -> bool {
        for pair in edits.windows(2) {
            if pair[1].range.start() < pair[0].range.end() {
                return false;
            }
        }

        return true;
    }
}

mod args {
    use super::Options;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    const HELP_PREFIX: &str = "\
expand selected Rust macros in a source file

usage:
";

    const HELP_SUFFIX: &str = "\
[OPTIONS] <SOURCE> [MACROS]...

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
  -h, --help                     print help.";

    fn help_text(bin_name: Option<&str>) -> String {
        format!(
            "{} {} {}",
            HELP_PREFIX,
            bin_name.unwrap_or("expandem"),
            HELP_SUFFIX
        )
    }

    pub(super) fn parse(
        bin_name: Option<String>,
        args: impl Iterator<Item = String>,
    ) -> Result<Options, (String, i32)> {
        let mut source: Option<PathBuf> = None;
        let mut macros: BTreeSet<String> = BTreeSet::new();
        let mut include_std = false;
        let mut include_core = false;
        let mut negate = false;
        let mut skip_build_scripts = false;
        let mut skip_proc_macros = false;
        let mut proc_macro_srv: Option<PathBuf> = None;

        let mut positional_only = false;
        let mut args = args.peekable();

        while let Some(arg) = args.next() {
            if positional_only || !arg.starts_with('-') || arg == "-" {
                if source.is_none() {
                    source = Some(PathBuf::from(arg));
                }
                else {
                    macros.insert(arg);
                }
                continue;
            }

            match arg.as_str() {
                "--" => positional_only = true,
                "-h" | "--help" => {
                    return Err((help_text(bin_name.as_deref()), 0));
                }
                "--include-std" => include_std = true,
                "--include-core" => include_core = true,
                "-n" | "--negate" => negate = true,
                "--skip-build-scripts" => skip_build_scripts = true,
                "--skip-proc-macros" => skip_proc_macros = true,
                "--proc-macro-srv" => {
                    let val = args.next().ok_or_else(|| {
                        ("--proc-macro-srv requires a value".to_string(), 2)
                    })?;
                    proc_macro_srv = Some(PathBuf::from(val));
                }
                _ if arg.starts_with("--proc-macro-srv=") => {
                    let val = arg.split_once('=').unwrap().1;
                    proc_macro_srv = Some(PathBuf::from(val));
                }
                _ => return Err((format!("unexpected argument '{arg}'"), 2)),
            }
        }

        let source = source.ok_or_else(|| {
            (format!(
                "the following required argument was not provided: <SOURCE>\n\n{}",
                help_text(bin_name.as_deref())
            ), 2)
        })?;

        if let Some(bad) = macros.iter().find(|it| it.ends_with('!')) {
            return Err((
                format!("macro paths must not end with `!`: {bad}"),
                2,
            ));
        }

        Ok(Options {
            source,
            macros,
            include_std,
            include_core,
            negate,
            skip_build_scripts,
            skip_proc_macros,
            proc_macro_srv,
        })
    }
}

mod failure {
    use std::error::Error;
    use std::fmt::{
        Display,
        Formatter,
    };

    #[derive(Debug)]
    pub enum ExpandemError {
        ExpandemError(String),
        Other(Box<dyn Error>),
    }

    impl ExpandemError {
        pub(crate) fn wrap<E>(it: E) -> Self
        where
            E: Error + 'static,
        {
            return ExpandemError::Other(Box::new(it));
        }

        pub(crate) fn of_str<E>(it: E) -> Self
        where
            E: AsRef<str>,
        {
            return Self::of_string(it.as_ref().to_string());
        }

        pub(crate) fn of_string(it: String) -> Self {
            return ExpandemError::ExpandemError(it);
        }

        pub(crate) fn fail<T, E>(it: E) -> Result<T, Self>
        where
            E: AsRef<str>,
        {
            return Err(Self::of_str(it));
        }
    }

    impl From<&str> for ExpandemError {
        fn from(value: &str) -> Self {
            return Self::of_str(value);
        }
    }

    impl From<String> for ExpandemError {
        fn from(value: String) -> Self {
            return Self::of_string(value);
        }
    }

    impl Display for ExpandemError {
        fn fmt(
            &self,
            f: &mut Formatter<'_>,
        ) -> std::fmt::Result {
            match self {
                ExpandemError::ExpandemError(it) => write!(f, "{}", it),
                ExpandemError::Other(it) => write!(f, "{}", it),
            }
        }
    }

    impl Error for ExpandemError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            return match self {
                Self::ExpandemError(_) => None,
                Self::Other(error) => Some(error.as_ref()),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_own_binary_source() {
        let output = expand(Options {
            source: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/main.rs"),
            skip_build_scripts: false,
            skip_proc_macros: false,
            ..Default::default()
        })
        .expect("the crate's binary source expands");

        assert!(output.contains("fn main()"));
    }
}
