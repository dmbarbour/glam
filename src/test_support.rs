//! Test-only control-flow helpers which do not accidentally observe semantic
//! payloads through Rust's formatting traits.

pub(crate) trait ResultTestExt<T, E> {
    /// Extracts the success without requiring the failure payload to
    /// implement `Debug`.
    #[track_caller]
    fn expect_without_debug(self, message: &str) -> T;

    /// Extracts the success without requiring the failure payload to
    /// implement `Debug`.
    #[track_caller]
    fn unwrap_without_debug(self) -> T;

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
    fn expect_without_debug(self, message: &str) -> T {
        match self {
            Ok(value) => value,
            Err(_) => panic!("{message}"),
        }
    }

    fn unwrap_without_debug(self) -> T {
        match self {
            Ok(value) => value,
            Err(_) => panic!("called `Result::unwrap_without_debug()` on an `Err` value"),
        }
    }

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

    #[test]
    fn success_extractors_do_not_require_debug_on_failure() {
        assert_eq!(
            Result::<_, NoDebug>::Ok(13).expect_without_debug("expected success"),
            13
        );
        assert_eq!(Result::<_, NoDebug>::Ok(17).unwrap_without_debug(), 17);
    }
}
