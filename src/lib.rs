#![allow(clippy::needless_return)]

pub use failure::ExpandemError;

#[cfg(test)]
mod tests;

const DEFAULT_MAX_EXPANSION_DEPTH: usize = 128;

#[derive(Debug, Clone)]
pub struct Options {
    /// Source file to transform.
    pub source: std::path::PathBuf,

    /// Path to rust-analyzer's proc-macro server
    pub proc_macro_srv: Option<std::path::PathBuf>,

    /// Paths of macros to include or exclude. An empty set selects every macro.
    /// A trailing `::*` matches macros directly on a path; trailing `::**`
    /// also matches macros on nested paths.
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

    /// Maximum nesting depth of selected macro expansions.
    pub max_depth: usize,
}

pub fn expand(it: Options) -> Result<String, ExpandemError> {
    return internal::expand(it);
}

pub fn parse_options(
    bin_name: Option<&str>,
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
        ModuleDef,
        PathResolution,
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

    fn substr(it: impl Display) -> String {
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

            let attribute_edits = selected_attribute_edits(
                &self.editor,
                &self.krate,
                syntax,
                syntax,
                0,
            )?;

            let ranges = attribute_edits
                .iter()
                .map(|edit| edit.range)
                .collect::<Vec<_>>();

            let derive_edits = self.editor.selected_derive_edits(
                &self.krate,
                syntax,
                &ranges,
                0,
            )?;

            let function_edits = self.editor.outermost_selected_calls_edits(
                &self.krate,
                syntax,
                syntax,
                ranges,
                0,
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
    }

    fn selected_attribute_edits(
        editor: &Editor<'_>,
        krate: &Crate,
        semantic_root: &SyntaxNode,
        output_root: &SyntaxNode,
        depth: usize,
    ) -> Result<Vec<Edit>, ExpandemError> {
        trace!("finding edit candidates");
        let mut output_items =
            output_root.descendants().filter_map(ast::Item::cast);
        let mut candidates = Vec::new();

        for item in semantic_root.descendants().filter_map(ast::Item::cast) {
            trace!("trying to expand: {}", substr(&item));
            let output_item = output_items.next().ok_or_else(|| {
                ExpandemError::of_str(
                    "macro expansion prettification changed item structure",
                )
            })?;

            let m_def = match editor.sema.resolve_attr_macro_call(&item) {
                Some(it) => it,
                None => continue,
            };

            let m_attr = match item.attrs().find(|attr| {
                let Some(path) = attr.meta().and_then(|meta| meta.path())
                else {
                    return false;
                };

                matches!(
                    editor.sema.resolve_path(&path),
                    Some(PathResolution::Def(ModuleDef::Macro(it)))
                        if it == m_def
                )
            }) {
                Some(it) => it,
                None => continue,
            };

            let m_path = match m_attr.meta().and_then(|meta| meta.path()) {
                Some(it) => it,
                None => continue,
            };

            if !editor.selects(&m_path.to_string(), editor.origin(m_def)) {
                trace!("not selected: {}", m_path);
                continue;
            }

            let expansion = match editor.sema.expand_attr_macro(&item) {
                None => {
                    trace!("did not expand: {}", substr(&item));
                    continue;
                }
                Some(it) => it,
            };

            if let Some(err) = expansion.err {
                warn!("expansion error: {:?}", err);
            }

            let attributes = item
                .attrs()
                .filter(|it| {
                    if it.syntax().text_range() == m_attr.syntax().text_range()
                    {
                        return false;
                    }

                    let Some(path) = it.meta().and_then(|meta| meta.path())
                    else {
                        return true;
                    };

                    let Some(PathResolution::Def(ModuleDef::Macro(def))) =
                        editor.sema.resolve_path(&path)
                    else {
                        return false;
                    };

                    !editor.selects(&path.to_string(), editor.origin(def))
                })
                .collect::<Vec<_>>();

            candidates.push((
                output_item.syntax().text_range(),
                expansion.value.value,
                attributes,
                editor.next_level(depth)?,
            ));
        }

        if output_items.next().is_some() {
            return ExpandemError::fail(
                "macro expansion prettification changed item structure",
            );
        }

        candidates.sort_unstable_by_key(|(range, _, _, _)| {
            (range.start(), std::cmp::Reverse(range.end()))
        });

        trace!("candidates: {:?}", candidates);

        let mut edits = Vec::new();
        for (range, expansion, attributes, expansion_depth) in candidates {
            if !edits.iter().any(|it: &Edit| covers(it.range, range)) {
                let preserved = missing_attributes(&attributes, &expansion);
                let mut replacement = editor.expand_selected_node(
                    krate,
                    &expansion,
                    expansion_depth,
                )?;
                if !preserved.is_empty() {
                    replacement =
                        format!("{}\n{}", preserved.join("\n"), replacement);
                }

                edits.push(Edit { range, replacement });
            }
        }

        trace!("edits: {:?}", edits);
        return Ok(edits);
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
        fn next_level(
            &self,
            depth: usize,
        ) -> Result<usize, ExpandemError> {
            let next = depth.checked_add(1).unwrap();
            if next > self.options.max_depth {
                return ExpandemError::fail(format!(
                    "macro expansion depth exceeds the configured limit of {}",
                    self.options.max_depth,
                ));
            }

            return Ok(next);
        }

        fn expand_selected_node(
            &self,
            krate: &Crate,
            expanded: &SyntaxNode,
            depth: usize,
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

            let mut edits = selected_attribute_edits(
                self, krate, expanded, &output, depth,
            )?;
            let attribute_ranges =
                edits.iter().map(|edit| edit.range).collect::<Vec<_>>();
            edits.extend(self.outermost_selected_calls_edits(
                krate,
                expanded,
                &output,
                attribute_ranges,
                depth,
            )?);

            edits.sort_unstable_by_key(|edit| {
                std::cmp::Reverse((edit.range.start(), edit.range.end()))
            });
            if !is_disjoint(&edits) {
                return ExpandemError::fail("macro expansions overlap");
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
            depth: usize,
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
                        meta.path().is_some_and(|p| p.to_string() == "derive")
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
                    .filter(|(it, ..)| !it.is_empty())
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
                                    self.next_level(depth)?,
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
            depth: usize,
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
                        self.next_level(depth)?,
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
                        || self.options.macros.contains(path)
                        || self
                            .options
                            .macros
                            .iter()
                            .filter(|selector| selector.ends_with('*'))
                            .any(|selector| macro_path_matches(selector, path));
                    if self.options.negate {
                        cond = !cond;
                    }
                    cond
                }
            };
        }
    }

    pub(super) fn macro_path_matches(
        selector: &str,
        path: &str,
    ) -> bool {
        if selector == path {
            return true;
        }

        if selector == "**" {
            return true;
        }
        if selector == "*" {
            return !path.contains("::");
        }

        if let Some(prefix) = selector.strip_suffix("::**") {
            return path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with("::") && rest.len() > 2);
        }
        if let Some(prefix) = selector.strip_suffix("::*") {
            return path.strip_prefix(prefix).is_some_and(|rest| {
                rest.starts_with("::")
                    && rest.len() > 2
                    && !rest[2..].contains("::")
            });
        }

        false
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

    fn missing_attributes(
        attributes: &[ast::Attr],
        expansion: &SyntaxNode,
    ) -> Vec<String> {
        let output_item = ast::Item::cast(expansion.clone())
            .or_else(|| expansion.descendants().find_map(ast::Item::cast));
        let mut output_attributes = output_item
            .into_iter()
            .flat_map(|item| item.attrs())
            .map(|attr| attribute_key(&attr))
            .collect::<Vec<_>>();

        attributes
            .iter()
            .filter_map(|attr| {
                let key = attribute_key(attr);
                if let Some(index) =
                    output_attributes.iter().position(|output| *output == key)
                {
                    output_attributes.remove(index);
                    None
                }
                else {
                    Some(attr.syntax().text().to_string())
                }
            })
            .collect()
    }

    fn attribute_key(attr: &ast::Attr) -> String {
        attr.syntax()
            .descendants_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(|token| !token.kind().is_trivia())
            .map(|token| token.text().to_string())
            .collect()
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
            if pair[1].range.end() > pair[0].range.start() {
                return false;
            }
        }

        return true;
    }
}

mod args {
    use super::{
        DEFAULT_MAX_EXPANSION_DEPTH,
        Options,
    };
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    const HELP_PREFIX: &str = "\
expand Rust macros

usage:
";

    const HELP_SUFFIX: &str = concat!("\
[OPTIONS] <SOURCE> [MACROS]...

arguments:
  <SOURCE>     source file to transform.
  [MACROS]...  macro paths to expand; trailing `*` matches that path and trailing `**` also matches nested paths.
               do not include `!`. omit to expand all macros.

options:
      --include-std              expand macros defined by the `std` crate.
      --include-core             expand macros defined by the `core` crate.
  -n, --negate                   treat MACROS as exclusions rather than inclusions.
      --skip-build-scripts       skip running build.rs scripts and proc macros.
      --skip-proc-macros         do not start the proc-macro server or expand proc macros.
      --proc-macro-srv <PATH>    path to rust-analyzer's proc-macro server.
      --max-depth <N>            maximum expansion depth.
  -h, --help                     print help.

  Version: ", env!("CARGO_PKG_VERSION"));

    fn help_text(bin_name: Option<&str>) -> String {
        format!(
            "{} {} {}",
            HELP_PREFIX,
            bin_name.unwrap_or("expandem"),
            HELP_SUFFIX
        )
    }

    pub(super) fn parse(
        bin_name: Option<&str>,
        args: impl Iterator<Item = String>,
    ) -> Result<Options, (String, i32)> {
        let mut source = None;
        let mut this = Options {
            source: "/".into(),
            macros: BTreeSet::new(),
            include_std: false,
            include_core: false,
            negate: false,
            skip_build_scripts: false,
            skip_proc_macros: false,
            proc_macro_srv: None,
            max_depth: DEFAULT_MAX_EXPANSION_DEPTH,
        };

        let mut positional_only = false;
        let mut args = args.peekable();

        while let Some(arg) = args.next() {
            if positional_only || !arg.starts_with('-') || arg == "-" {
                if source.is_none() {
                    source = Some(PathBuf::from(arg));
                }
                else {
                    this.macros.insert(arg);
                }
                continue;
            }

            match arg.as_str() {
                "-h" | "--help" => return Err((help_text(bin_name), 0)),
                "--" => positional_only = true,
                "--include-std" => this.include_std = true,
                "--include-core" => this.include_core = true,
                "-n" | "--negate" => this.negate = true,
                "--skip-build-scripts" => this.skip_build_scripts = true,
                "--skip-proc-macros" => this.skip_proc_macros = true,
                "--proc-macro-srv" => {
                    let val = args.next().ok_or_else(|| {
                        ("--proc-macro-srv requires a value".to_string(), 2)
                    })?;
                    this.proc_macro_srv = Some(PathBuf::from(val));
                }
                "--max-depth" => {
                    let val = args.next().ok_or_else(|| {
                        ("--max-depth requires a value".to_string(), 2)
                    })?;
                    this.max_depth = val
                        .parse()
                        .map(|it| match it {
                            0 => usize::MAX,
                            _ => it,
                        })
                        .map_err(|_| {
                            (
                                "--max-depth must be a non-negative integer"
                                    .to_string(),
                                2,
                            )
                        })?;
                }
                _ => return Err((format!("unexpected argument '{arg}'"), 2)),
            }
        }

        this.source = source.ok_or_else(|| {
            (format!(
                "the following required argument was not provided: <SOURCE>\n\n{}",
                help_text(bin_name)
            ), 2)
        })?;

        if let Some(bad) = this.macros.iter().find(|it| it.ends_with('!')) {
            return Err((
                format!("macro paths must not end with `!`: {bad}"),
                2,
            ));
        }

        return Ok(this);
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
