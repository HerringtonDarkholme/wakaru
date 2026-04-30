use std::path::PathBuf;

use wakaru_core::diagnostics::Result;
use wakaru_core::source::SourceFile;

pub(crate) fn define_inline_test(
    transform: fn(&SourceFile) -> Result<String>,
) -> impl Fn(&str, &str) {
    move |input, expected| {
        let source = SourceFile::from_parts(PathBuf::from("test.js"), input);
        let output = transform(&source).expect("transform should succeed");

        assert_eq!(normalize(&output), normalize(expected));
    }
}

fn normalize(code: &str) -> String {
    code.trim().replace("\r\n", "\n")
}
