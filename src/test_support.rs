//! Test-only control-flow helpers which do not accidentally observe semantic
//! payloads through Rust's formatting traits.

pub(crate) trait ResultTestExt<T, E> {
    /// Extracts the failure without requiring the successful payload to
    /// implement `Debug`.
    #[track_caller]
    fn expect_err_without_debug(self, message: &str) -> E;

    /// Extracts the failure without requiring the successful payload to
    /// implement `Debug`.
    #[track_caller]
    fn unwrap_err_without_debug(self) -> E;
}

impl<T, E> ResultTestExt<T, E> for Result<T, E> {
    fn expect_err_without_debug(self, message: &str) -> E {
        match self {
            Err(error) => error,
            Ok(_) => panic!("{message}"),
        }
    }

    fn unwrap_err_without_debug(self) -> E {
        match self {
            Err(error) => error,
            Ok(_) => panic!("called `Result::unwrap_err_without_debug()` on an `Ok` value"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ResultTestExt as _;

    struct NoDebug;

    #[test]
    fn error_extractors_do_not_require_debug_on_success() {
        assert_eq!(
            Result::<NoDebug, _>::Err(7).expect_err_without_debug("expected failure"),
            7
        );
        assert_eq!(Result::<NoDebug, _>::Err(11).unwrap_err_without_debug(), 11);
    }
}
