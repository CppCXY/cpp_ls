use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::PathBuf;
use std::collections::HashMap;

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("a directory"));
    let file = root.join("main.cpp");
    let mut session = Session::open(root.clone(), SessionFiles::new(OpenDocuments::new(), DiskFiles), WatchFilter::new(&root));
    session.index_everything();
    let index = session.index();
    println!("summaries {}", index.len());
    println!("visible_files from main {}", index.visible_files(&file).len());
    if let Some(stream) = session.render_the_unit(&session.index().summary(&file).unwrap().path.clone()) { std::fs::write("target/stream.cpp", &stream.text).unwrap(); println!("stream bytes {}", stream.text.len()); }
    if let Some(stream) = session.render_the_unit(&session.index().summary(&file).unwrap().path.clone()) {
        let mut per: std::collections::BTreeMap<String, usize> = Default::default();
        for span in &stream.spans { *per.entry(stream.files[span.file as usize].file_name().unwrap().to_string_lossy().to_string()).or_default() += 1; }
        let mut v: Vec<_> = per.into_iter().collect(); v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        println!("TOKENS per file {:?}", &v[..v.len().min(12)]);
        for want in ["vector", "memory", "format", "string", "main.cpp"] { println!("TOK {want}: {:?}", v.iter().find(|(n, _)| n == want)); }
    }
    let reading = session.read_the_unit(&file);
    println!("unit reading {:?}", reading.as_ref().map(|r| (r.files, r.tokens, r.errors)));
    let index = session.index();
    let mut per_file: Vec<(usize, usize, String)> = Vec::new();
    let mut total_raw = 0; let mut total_cooked = 0;
    for summary in index.summaries() {
        let raw = summary.declarations.iter().filter(|d| d.scope.as_deref() == Some("std")).count();
        let cooked = index.cooked_declarations(&summary.path).map(|c| c.iter().filter(|d| d.scope.as_deref() == Some("std")).count()).unwrap_or(0);
        total_raw += raw; total_cooked += cooked;
        per_file.push((raw, cooked, summary.path.display().to_string()));
    }
    println!("std-scope decls: raw {total_raw}, cooked {total_cooked}");
    per_file.sort_by_key(|(r, c, _)| std::cmp::Reverse(r + c));
    for (r, c, p) in per_file.iter().take(15) { println!("{r:>5} {c:>5} {p}"); }
    let mut by_kind: HashMap<String, usize> = HashMap::new();
    for summary in index.summaries() { for d in &summary.declarations { *by_kind.entry(format!("{:?}", d.kind)).or_default() += 1; } }
    println!("{:?}", by_kind);
    let mut scopes: HashMap<String, usize> = HashMap::new(); let mut ncooked = 0;
    for summary in index.summaries() { if let Some(c) = index.cooked_declarations(&summary.path) { ncooked += c.len(); for d in c { *scopes.entry(d.scope.clone().unwrap_or_default().to_string()).or_default() += 1; } } }
    let mut sv: Vec<_> = scopes.into_iter().collect(); sv.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("cooked total {ncooked}; top scopes {:?}", &sv[..sv.len().min(12)]);
    for summary in index.summaries().filter(|s| s.path.to_string_lossy().ends_with("xstring")) { println!("xstring raw {} cooked {:?}", summary.declarations.len(), index.cooked_declarations(&summary.path).map(|c| c.len())); }
    for want in ["vector", "format", "unique_ptr", "size_t", "string", "basic_string_view"] {
        for summary in index.summaries() { if let Some(c) = index.cooked_declarations(&summary.path) { for d in c { if &*d.name == want { println!("  cooked {want}: scope {:?} kind {:?} in {}", d.scope, d.kind, summary.path.file_name().unwrap().to_string_lossy()); } } } }
    }
    if let Ok(which) = std::env::var("DUMP_FILE") { for summary in index.summaries().filter(|s| s.path.to_string_lossy().ends_with(&which)) { println!("== {}", summary.path.display()); if let Some(c) = index.cooked_declarations(&summary.path) { for d in c.iter().take(80) { println!("  {:?} {:?} scope={:?}", d.kind, d.name, d.scope); } println!("  total {}", c.len()); } } }
    for summary in index.summaries() { let p = summary.path.to_string_lossy(); if ["/vector","/memory","/format","/string","/xstring","/xmemory"].iter().any(|s| p.ends_with(s)) { println!("FILE {} raw {} cooked {:?}", p.rsplit('/').next().unwrap(), summary.declarations.len(), index.cooked_declarations(&summary.path).map(|c| c.len())); } }
    println!("visible std from main: {}", index.declarations_in("std", &file).len());
    for name in ["std::string", "std::basic_string", "std::format", "std::vector", "std::size_t", "std::unique_ptr"] {
        println!("{name}: {:?}", format!("{:?}", index.definition(name, &file)).chars().take(90).collect::<String>());
    }
}







