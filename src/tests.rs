#[cfg(test)]
mod tests {
    use crate::internal::{
        macro_path_matches,
        remove_redundant_delimiters,
        simplify_path_prefixes,
        simplify_syntax,
        unwrap_literal_true_branches,
    };
    use ra_ap_ide::RootDatabase;
    use ra_ap_ide_db::base_db::SourceDatabase;
    use ra_ap_syntax::{
        AstNode,
        Edition,
        NodeOrToken,
        SourceFile,
        SyntaxNode,
        ast::{
            self,
            HasGenericArgs,
            HasName,
        },
    };
    use ra_ap_test_fixture::WithFixture;

    fn unwrap_true_branches(source: &str) -> String {
        return unwrap_literal_true_branches(source.to_owned());
    }

    #[test]
    fn matches_exact_macro_paths() {
        assert!(macro_path_matches(
            "crate::module::mac",
            "crate::module::mac"
        ));
        assert!(!macro_path_matches(
            "crate::module::mac",
            "crate::module::other"
        ));
    }

    #[test]
    fn single_wildcard_matches_only_macros_directly_on_path() {
        assert!(macro_path_matches("crate::module::*", "crate::module::mac"));
        assert!(!macro_path_matches(
            "crate::module::*",
            "crate::module::nested::mac",
        ));
        assert!(!macro_path_matches("crate::module::*", "crate::module"));
        assert!(!macro_path_matches(
            "crate::module::*",
            "crate::modules::mac"
        ));
        assert!(macro_path_matches("*", "mac"));
        assert!(!macro_path_matches("*", "module::mac"));
    }

    #[test]
    fn double_wildcard_matches_macros_on_or_below_path() {
        assert!(macro_path_matches(
            "crate::module::**",
            "crate::module::mac"
        ));
        assert!(macro_path_matches(
            "crate::module::**",
            "crate::module::nested::mac",
        ));
        assert!(!macro_path_matches("crate::module::**", "crate::module"));
        assert!(!macro_path_matches(
            "crate::module::**",
            "crate::modules::mac"
        ));
        assert!(macro_path_matches("**", "mac"));
        assert!(macro_path_matches("**", "module::mac"));
    }

    #[test]
    fn wildcard_is_special_only_as_the_last_path_element() {
        assert!(!macro_path_matches("crate::*::mac", "crate::module::mac"));
        assert!(!macro_path_matches("crate::mod*", "crate::module"));
    }

    #[test]
    fn simplifies_literal_true_in_expression_positions() {
        let source = r#"const _: () = {
    struct Value;
    impl Value {
        fn result() -> Option<u32> {
            return if true {
                Some(if true { 1 } else { 2 })
            } else {
                if true { None } else { unreachable!() }
            };
        }
    }
};

const NUMBER: u32 = if true { 3 } else { 4 };

fn main() {
    let number = if true { 5 } else { 6 };
    consume(if true { number } else { 7 });
    let closure = || if true { 8 } else { 9 };
    let sum = 10 + if true { 11 } else { 12 };
    if true { consume(sum); }
}
"#;
        let output = unwrap_true_branches(source);
        let parsed = SourceFile::parse(&output, Edition::CURRENT);

        assert!(parsed.errors().is_empty(), "{output}");
        assert!(
            !parsed
                .tree()
                .syntax()
                .descendants()
                .any(|node| { ast::IfExpr::can_cast(node.kind()) })
        );
    }

    #[test]
    fn preserves_selected_block_scope() {
        let source = r#"fn main() {
    let value = 1;
    if true {
        let value = 2;
        consume(value);
    } else {
        discarded();
    }
    consume(value);
}
"#;
        let output = unwrap_true_branches(source);
        let parsed = SourceFile::parse(&output, Edition::CURRENT);
        assert!(parsed.errors().is_empty(), "{output}");
        let function = parsed
            .tree()
            .syntax()
            .descendants()
            .find_map(ast::Fn::cast)
            .unwrap();
        let statements = function.body().unwrap().stmt_list().unwrap();
        let declarations = statements
            .syntax()
            .children()
            .filter_map(ast::LetStmt::cast)
            .count();
        let selected = statements
            .syntax()
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .unwrap();

        assert_eq!(declarations, 1);
        assert!(selected.stmt_list().unwrap().statements().any(|statement| {
            ast::LetStmt::can_cast(statement.syntax().kind())
        }));
    }

    #[test]
    fn preserves_outer_condition_when_simplifying_else_if() {
        let source = r#"fn value(condition: bool) -> u32 {
    if condition {
        1
    } else if true {
        2
    } else {
        3
    }
}
"#;
        let output = unwrap_true_branches(source);
        let parsed = SourceFile::parse(&output, Edition::CURRENT);
        assert!(parsed.errors().is_empty(), "{output}");
        let conditions = parsed
            .tree()
            .syntax()
            .descendants()
            .filter_map(ast::IfExpr::cast)
            .collect::<Vec<_>>();

        assert_eq!(conditions.len(), 1);
        assert!(matches!(
            conditions[0].condition(),
            Some(ast::Expr::PathExpr(_))
        ));
        assert!(ast::BlockExpr::can_cast(
            conditions[0].else_branch().unwrap().syntax().kind()
        ));
    }

    #[test]
    fn preserves_attributes_on_selected_branches() {
        use ast::HasAttrs;

        let source = r#"fn main() {
    #[cfg(feature = "selected")]
    if true { selected(); } else { discarded(); }
}
"#;
        let output = unwrap_true_branches(source);
        let parsed = SourceFile::parse(&output, Edition::CURRENT);
        assert!(parsed.errors().is_empty(), "{output}");
        let selected = parsed
            .tree()
            .syntax()
            .descendants()
            .filter_map(ast::BlockExpr::cast)
            .find(|block| block.attrs().next().is_some())
            .unwrap();

        assert_eq!(selected.attrs().count(), 1);
    }

    #[test]
    fn leaves_non_literal_true_conditions_unchanged() {
        let source = r#"fn value(condition: bool) -> i32 {
    if condition {
        1
    } else if false {
        2
    } else {
        3
    }
}
"#;

        assert_eq!(unwrap_true_branches(source), source);
    }

    fn syntax_shape(
        node: &SyntaxNode,
        shape: &mut Vec<String>,
    ) {
        let parenthesized = ast::ParenExpr::can_cast(node.kind())
            || ast::ParenType::can_cast(node.kind());
        if !parenthesized {
            shape.push(format!("{:?}", node.kind()));
        }
        for element in node.children_with_tokens() {
            match element {
                NodeOrToken::Node(child) => syntax_shape(&child, shape),
                NodeOrToken::Token(token) if !token.kind().is_trivia() => {
                    if !parenthesized || !matches!(token.text(), "(" | ")") {
                        shape.push(token.text().to_owned());
                    }
                }
                _ => {}
            }
        }
        if !parenthesized {
            shape.push("end".to_owned());
        }
    }

    #[test]
    fn removes_redundant_parentheses_without_changing_expression_structure() {
        let source = r#"struct Callable { invoke: fn() -> i32 }
fn value(x: i32, y: i32, p: &i32, callable: Callable) {
    let _ = (((x)));
    let _ = (x * y) + 1;
    let _ = x - (y - 1);
    let _ = ((x + 1)) * y;
    let _ = (x as u32) < (y as u32);
    let _ = (x..y).contains(&2);
    let _ = (|| x)();
    let _ = (callable.invoke)();
    let _ = x/(*p);
    let _ = (x)as u32;
    let _ = (x,);
    if(x > 0) { consume(x); }
}
"#;
        let output = remove_redundant_delimiters(source.to_owned());
        let original = SourceFile::parse(source, Edition::CURRENT);
        let parsed = SourceFile::parse(&output, Edition::CURRENT);
        assert!(original.errors().is_empty(), "{source}");
        assert!(parsed.errors().is_empty(), "{output}");
        let mut before = Vec::new();
        let mut after = Vec::new();
        syntax_shape(original.tree().syntax(), &mut before);
        syntax_shape(parsed.tree().syntax(), &mut after);
        assert_eq!(before, after);
        for parens in parsed
            .tree()
            .syntax()
            .descendants()
            .filter_map(ast::ParenExpr::cast)
        {
            assert!(parens.expr().unwrap().needs_parens_in_place_of(
                &parens.syntax().parent().unwrap(),
                parens.syntax(),
            ));
        }
    }

    #[test]
    fn removes_type_parentheses_but_preserves_trait_object_bounds() {
        let source =
            "type Plain = (((u32))); type Object = &((dyn Trait + Send));";
        let output = remove_redundant_delimiters(source.to_owned());
        let original = SourceFile::parse(source, Edition::CURRENT);
        let parsed = SourceFile::parse(&output, Edition::CURRENT);
        assert!(parsed.errors().is_empty(), "{output}");
        let mut before = Vec::new();
        let mut after = Vec::new();
        syntax_shape(original.tree().syntax(), &mut before);
        syntax_shape(parsed.tree().syntax(), &mut after);
        assert_eq!(before, after);
        for parens in parsed
            .tree()
            .syntax()
            .descendants()
            .filter_map(ast::ParenType::cast)
        {
            assert!(
                parens
                    .ty()
                    .unwrap()
                    .needs_parens_in(&parens.syntax().parent().unwrap())
            );
        }
    }

    #[test]
    fn retains_attributes_and_comments_during_parentheses_cleanup() {
        let source = r#"fn value() {
    let _ = #[cfg(any())] (1 + 2);
    let _ = (/* keep this */ 3);
    example!((untouched));
}
"#;
        let output = remove_redundant_delimiters(source.to_owned());
        let original = SourceFile::parse(source, Edition::CURRENT);
        let parsed = SourceFile::parse(&output, Edition::CURRENT);
        assert!(parsed.errors().is_empty(), "{output}");
        let original_comments = original
            .tree()
            .syntax()
            .descendants_with_tokens()
            .filter(|token| token.kind() == ra_ap_syntax::SyntaxKind::COMMENT)
            .map(|token| token.to_string())
            .collect::<Vec<_>>();
        let comments = parsed
            .tree()
            .syntax()
            .descendants_with_tokens()
            .filter(|token| token.kind() == ra_ap_syntax::SyntaxKind::COMMENT)
            .map(|token| token.to_string())
            .collect::<Vec<_>>();
        assert_eq!(comments, original_comments);
        let mut before = Vec::new();
        let mut after = Vec::new();
        syntax_shape(original.tree().syntax(), &mut before);
        syntax_shape(parsed.tree().syntax(), &mut after);
        assert_eq!(before, after);
    }

    #[test]
    fn removes_redundant_return_blocks() {
        let source = r#"struct Counter(core::num::Wrapping<u32>);
impl Counter {
    fn try_map(value: u32) -> Result<Self, u32> {
        return{Ok(Self(core::num::Wrapping(value)))};
    }
}
fn nested(value: u32) -> u32 { return{{(value)}}; }
fn commented(value: u32) -> u32 {
    return{/* before */value/* after */};
}
"#;
        let output = simplify_syntax(source.to_owned());
        let parsed = SourceFile::parse(&output, Edition::CURRENT);
        assert!(parsed.errors().is_empty(), "{output}");
        let expressions = parsed
            .tree()
            .syntax()
            .descendants()
            .filter_map(ast::ReturnExpr::cast)
            .map(|expr| expr.expr().unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(expressions[0], ast::Expr::CallExpr(_)));
        assert!(matches!(expressions[1], ast::Expr::PathExpr(_)));
        assert!(matches!(expressions[2], ast::Expr::PathExpr(_)));
        let comments = parsed
            .tree()
            .syntax()
            .descendants_with_tokens()
            .filter(|token| token.kind() == ra_ap_syntax::SyntaxKind::COMMENT)
            .map(|token| token.to_string())
            .collect::<Vec<_>>();
        assert_eq!(comments, ["/* before */", "/* after */"]);
        assert_eq!(simplify_syntax(output.clone()), output);
    }

    #[test]
    fn preserves_required_return_blocks() {
        let source = r#"fn statements(value: u32) -> u32 {
    return { let value = value + 1; value };
}
fn items() -> u32 { return { const VALUE: u32 = 1; VALUE }; }
fn unit() { return { consume(); }; }
fn empty() { return {}; }
fn unsafe_value(pointer: *const u32) -> u32 { return unsafe { *pointer }; }
fn constant() -> u32 { return const { 1 }; }
fn future() -> impl Future<Output = u32> { return async move { 1 }; }
fn fallible() -> Result<u32, ()> { return try { operation()? }; }
fn labeled() -> u32 { return 'value: { break 'value 1 }; }
fn outer() -> u32 { return { #[allow(unused)] { 1 } }; }
fn inner() -> u32 { return { { #![allow(unused)] 1 } }; }
fn documented() -> u32 { return { { //! keep scope
    1 } }; }
fn attributed_tail() -> u32 { return { #[allow(unused)] 1 }; }
fn operand(value: u32) -> u32 { return { value } + 1; }
fn unexpanded() { example!(return { untouched }); }
"#;
        let output = simplify_syntax(source.to_owned());
        let original = SourceFile::parse(source, Edition::CURRENT);
        let parsed = SourceFile::parse(&output, Edition::CURRENT);
        assert!(
            original.errors().is_empty(),
            "{:?}\n{source}",
            original.errors(),
        );
        assert!(parsed.errors().is_empty(), "{output}");
        let mut before = Vec::new();
        let mut after = Vec::new();
        syntax_shape(original.tree().syntax(), &mut before);
        syntax_shape(parsed.tree().syntax(), &mut after);
        assert_eq!(before, after);
    }

    #[test]
    fn combines_parentheses_and_literal_branch_cleanup() {
        let source =
            "fn value() -> u32 { return (if ((true)) { ((1)) } else { 2 }); }";
        let output = simplify_syntax(source.to_owned());
        let parsed = SourceFile::parse(&output, Edition::CURRENT);
        assert!(parsed.errors().is_empty(), "{output}");
        assert!(!parsed.tree().syntax().descendants().any(|node| {
            ast::IfExpr::can_cast(node.kind())
                || ast::ParenExpr::can_cast(node.kind())
        }));
        let returned = parsed
            .tree()
            .syntax()
            .descendants()
            .find_map(ast::ReturnExpr::cast)
            .unwrap();
        assert!(matches!(returned.expr(), Some(ast::Expr::Literal(_))));
    }

    fn simplified_paths(fixture: &str) -> SourceFile {
        let (mut db, files) = RootDatabase::with_many_files(fixture);
        let file_id = files[0].file_id(&db);
        let source = db.file_text(file_id).text(&db).to_string();
        let output = simplify_path_prefixes(source, &mut db, file_id);
        let parsed = SourceFile::parse(&output, Edition::CURRENT);
        assert!(parsed.errors().is_empty(), "{output}");
        return parsed.tree();
    }

    fn return_path(
        file: &SourceFile,
        name: &str,
    ) -> ast::Path {
        let function = file
            .syntax()
            .descendants()
            .filter_map(ast::Fn::cast)
            .find(|function| {
                function.name().is_some_and(|it| it.text() == name)
            })
            .unwrap();
        return function
            .ret_type()
            .unwrap()
            .syntax()
            .descendants()
            .find_map(ast::Path::cast)
            .unwrap();
    }

    #[test]
    fn shortens_resolved_paths_without_rewriting_imports_into_self_imports() {
        let file = simplified_paths(
            r#"
//- /main.rs crate:main deps:helper
use ::helper::types::Item;
fn item() -> ::helper::types::Item { return::helper::types::Item; }
//- /helper.rs crate:helper
pub mod types { pub struct Item; }
"#,
        );
        assert!(return_path(&file, "item").as_single_name_ref().is_some());
        let import = file
            .syntax()
            .descendants()
            .find_map(ast::Use::cast)
            .unwrap();
        let path = import
            .syntax()
            .descendants()
            .find_map(ast::Path::cast)
            .unwrap();
        assert_eq!(path.segments().count(), 3);
        assert!(path.first_segment().unwrap().coloncolon_token().is_none());
    }

    #[test]
    fn retains_absolute_paths_when_a_local_module_shadows_the_dependency() {
        let file = simplified_paths(
            r#"
//- /main.rs crate:main deps:helper
mod helper { pub mod types { pub struct Item; } }
fn item() -> ::helper::types::Item { ::helper::types::Item }
//- /helper.rs crate:helper
pub mod types { pub struct Item; }
"#,
        );
        let path = return_path(&file, "item");
        assert!(path.first_segment().unwrap().coloncolon_token().is_some());
        assert_eq!(path.segments().count(), 3);
    }

    #[test]
    fn does_not_shorten_paths_to_shadowing_type_parameters() {
        let file = simplified_paths(
            r#"
//- /main.rs crate:main deps:helper
use helper::types::Item;
fn item<Item>() -> ::helper::types::Item { ::helper::types::Item }
//- /helper.rs crate:helper
pub mod types { pub struct Item; }
"#,
        );
        let path = return_path(&file, "item");
        assert_eq!(path.segments().count(), 3);
        assert!(path.first_segment().unwrap().coloncolon_token().is_none());
    }

    #[test]
    fn retains_generic_arguments_in_qualified_associated_paths() {
        let file = simplified_paths(
            r#"
//- /main.rs crate:main deps:helper
use helper::Container;
fn value() -> u8 { ::helper::Container::<u8>::ID }
//- /helper.rs crate:helper
pub struct Container<T>(T);
impl<T> Container<T> { pub const ID: u8 = 1; }
"#,
        );
        let function =
            file.syntax().descendants().find_map(ast::Fn::cast).unwrap();
        let expression = function.body().unwrap().tail_expr().unwrap();
        let ast::Expr::PathExpr(expression) = expression
        else {
            panic!("expected associated path");
        };
        let path = expression.path().unwrap();
        assert_eq!(path.segments().count(), 2);
        assert!(path.first_segment().unwrap().generic_arg_list().is_some());
    }

    #[test]
    fn keeps_unresolved_path_prefixes() {
        let file = simplified_paths("fn item() -> ::missing::Item { loop {} }");
        let path = return_path(&file, "item");
        assert!(path.first_segment().unwrap().coloncolon_token().is_some());
        assert_eq!(path.segments().count(), 2);
    }
}
