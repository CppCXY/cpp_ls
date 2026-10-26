//! 诊断:一个文件被单独读取时,什么算它自己的错误。
//!
//! 这里只有一个问题 —— **fragment 头文件**:它的正文是一个初始化列表的一段,由另一个文件把花括号打开。
//! 单独读它会得到上百个语法错误,那些错误全是"没有开括号"的下游;文件自己写了 `#error` 说明这件事,
//! 我们就只说那一句。

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// **声明自己是 fragment 的头文件**:文件写了 `#error`,而且渲染的第一个 token 是 `{`。
///
/// libstdc++ 的 `bits/text_encoding-data.h` 就是这样 —— 它是一张表的一段,由 `text_encoding.cc` 在花括号里
/// 打开。单独读它是一个没有开括号的初始化列表:实测 895 个语法错误,全是那个 `{` 的下游。编译器只说一句,
/// 就是它自己写的 `#error` —— 我们也只说那一句。
#[test]
fn a_fragment_header_reports_its_own_refusal_not_the_cascade() {
    let fragment = "\
#ifndef I_AM_A_TABLE
# error \"This is not a public header, do not include it directly\"
#endif

  {    3, \"US-ASCII\" },
  {    3, \"iso-ir-6\" },
  {    3, \"ANSI_X3.4-1968\" },
";
    let main = "#include \"data.h\"\nint main() { return 0; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/data.h", fragment)
        .with_file("/p/main.cpp", main);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/main.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/main.cpp"));
    session.want_cooked_reading(std::path::Path::new("/p/data.h"));
    session.index_everything();

    let found = session
        .diagnostics("/p/data.h")
        .expect("the fragment has a reading");
    println!(
        "errors={} first={:?}",
        found.errors.len(),
        found
            .errors
            .first()
            .map(|error| (&error.message, error.start))
    );
    assert_eq!(
        found.errors.len(),
        1,
        "one refusal, not the cascade it causes: {:?}",
        found
            .errors
            .iter()
            .map(|error| error.message.as_str())
            .take(4)
            .collect::<Vec<_>>()
    );
    assert!(
        found.errors[0].message.contains("not a public header"),
        "{:?}",
        found.errors[0].message
    );
}
