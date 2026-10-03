#[cfg(test)]
mod tests {
    use crate::internal::macro_path_matches;

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
}
