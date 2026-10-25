#[cfg(test)]
mod tests {
    use crate::internal::{
        macro_path_matches,
        unwrap_literal_true_branches,
    };
    use ra_ap_vfs::AbsPathBuf;

    fn unwrap_true_branches(source: &str) -> String {
        return unwrap_literal_true_branches(
            source.to_owned(),
            AbsPathBuf::assert_utf8(std::env::current_dir().unwrap()),
        )
        .unwrap();
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
    fn unwraps_literal_true_statement_branches() {
        let source = r#"fn main() {
    before();
    if true {
        selected();
    } else {
        discarded();
    }
    after();
}
"#;
        let expected = r#"fn main() {
    before();
    selected();
    after();
}
"#;

        assert_eq!(unwrap_true_branches(source), expected);
    }

    #[test]
    fn unwraps_literal_true_expression_branches() {
        let source = r#"fn value() -> i32 {
    let value = if true {
        1
    } else {
        2
    };
    value
}
"#;
        let expected = r#"fn value() -> i32 {
    let value = 1;
    value
}
"#;

        assert_eq!(unwrap_true_branches(source), expected);
    }

    #[test]
    fn unwraps_nested_literal_true_branches() {
        let source = r#"fn value() -> i32 {
    if true {
        if true {
            1
        } else {
            2
        }
    } else {
        3
    }
}
"#;
        let expected = r#"fn value() -> i32 {
    1
}
"#;

        assert_eq!(unwrap_true_branches(source), expected);
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
}
