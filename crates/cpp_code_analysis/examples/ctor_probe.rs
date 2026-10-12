use cpp_code_analysis::{CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
fn main() {
    let memory = MemoryFiles::new()
        .with_file("/p/main.cpp", "int main() {}\n")
        .with_file("/p/CMakeLists.txt", "project(p)\n")
        .with_file("/p/build/CMakeCache.txt", "CMAKE_HOME_DIRECTORY:INTERNAL=/p\nCMAKE_EXPORT_COMPILE_COMMANDS:BOOL=OFF\n");
    for probe in ["/p/build/CMakeCache.txt", "p/build/CMakeCache.txt", "/p/CMakeLists.txt"] {
        println!("  read {probe:32?} -> {}", cpp_code_analysis::FileProvider::read(&memory, std::path::Path::new(probe)).is_some());
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.add_project_files([std::path::PathBuf::from("/p/main.cpp")]);
    session.index_everything();
    let d = session.discovery();
    println!("problems={:?}", d.problems.iter().map(|p| p.message.clone()).collect::<Vec<_>>());
    println!("cmake={:?}", d.cmake.as_ref().map(|c| c.path.clone()));
}
