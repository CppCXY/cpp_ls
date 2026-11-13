use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use cpp_code_analysis::{
    CompilerConfig, DiskFiles, FileEvent, OpenDocuments, Session, SessionFiles, WatchFilter,
    content_hash,
};

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "cpp-ls-publication-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).expect("a new project directory");
        Self(root)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, text).expect("write project file");
        path
    }

    fn session(&self) -> Session<DiskFiles> {
        Session::with_config(
            &self.0,
            SessionFiles::new(OpenDocuments::new(), DiskFiles),
            WatchFilter::new(&self.0),
            CompilerConfig::default(),
        )
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove the test project");
    }
}

fn declares(session: &Session<DiskFiles>, path: &Path, name: &str) -> bool {
    session
        .index()
        .summary(path)
        .is_some_and(|summary| summary.declarations.iter().any(|fact| fact.name == name))
}

#[test]
fn a_wave_prepared_before_a_watched_edit_is_discarded_and_retried() {
    let project = Project::new();
    let old = "int old_name;\n";
    let new = "int new_name;\n";
    let path = project.write("main.cpp", old);
    let mut session = project.session();
    session.add_project_files([path.clone()]);
    let wave = session.prepare_a_wave(1);
    assert!(wave.prepared.iter().any(|(file, _)| file == &path));

    std::fs::write(&path, new).expect("change the file");
    session.changed([FileEvent::modified(path.clone())]);
    let (done, _) = session.commit_a_wave(wave, 1);
    assert!(
        done.is_empty(),
        "the previous file version must not be committed"
    );
    assert!(!declares(&session, &path, "old_name"));
    assert!(session.pending() > 0, "the current file remains queued");

    session.advance(1);
    assert!(declares(&session, &path, "new_name"));
}

#[test]
fn a_watched_edit_invalidates_the_summary_and_held_disk_text() {
    let project = Project::new();
    let path = project.write("main.cpp", "int old_name;\n");
    let mut session = project.session();
    session.index_everything();
    assert!(declares(&session, &path, "old_name"));
    assert_eq!(
        session.files().held(&path).unwrap().text.as_ref(),
        "int old_name;\n"
    );

    std::fs::write(&path, "int new_name;\n").unwrap();
    session.changed([FileEvent::modified(path.clone())]);
    assert!(session.index().summary(&path).is_none());
    assert!(session.files().held(&path).is_none());
    session.catch_up(&path);
    assert!(declares(&session, &path, "new_name"));
    assert_eq!(
        session.files().held(&path).unwrap().text.as_ref(),
        "int new_name;\n"
    );
}

#[test]
fn an_edit_removes_the_previous_refinement_mark() {
    let project = Project::new();
    let path = project.write("main.cpp", "int old_name;\n");
    let mut session = project.session();
    session.did_open(&path, "int old_name;\n");
    session.want_refinement(&path);
    session.index_everything();
    assert!(session.is_refined(&path), "the initial reading is refined");

    session.did_change(&path, "int new_name;\n");
    assert!(
        !session.is_refined(&path),
        "the old refinement cannot describe the edit"
    );
}

#[test]
fn a_wave_from_a_replaced_session_cannot_be_committed() {
    let project = Project::new();
    let path = project.write("main.cpp", "int value;\n");
    let mut old = project.session();
    old.add_project_files([path.clone()]);
    let wave = old.prepare_a_wave(1);
    assert!(wave.prepared.iter().any(|(file, _)| file == &path));

    let mut current = project.session();
    current.add_project_files([path.clone()]);
    let (done, _) = current.commit_a_wave(wave, 1);
    assert!(done.is_empty());
    assert!(current.index().summary(&path).is_none());
    current.advance(1);
    assert!(declares(&current, &path, "value"));
}

#[test]
fn a_detached_file_read_is_discarded_after_an_edit() {
    let project = Project::new();
    let path = project.write("main.cpp", "int old_name;\n");
    let mut session = project.session();
    session.did_open(&path, "int old_name;\n");
    let old = session
        .gather_a_file_read(&path)
        .expect("the opened file is queued")
        .read();

    session.did_change(&path, "int new_name;\n");
    assert!(!session.commit_a_file_read(old));
    let current = session
        .gather_a_file_read(&path)
        .expect("the edited file is still queued")
        .read();
    assert!(session.commit_a_file_read(current));
    assert!(declares(&session, &path, "new_name"));
    assert!(!declares(&session, &path, "old_name"));
}

#[test]
fn background_waves_defer_open_files_without_starving_other_work() {
    let project = Project::new();
    let open = project.write("open.cpp", "int open_name;\n");
    let other = project.write("other.cpp", "int other_name;\n");
    let mut session = project.session();
    session.did_open(&open, "int open_name;\n");
    session.add_project_files([other.clone()]);

    let wave = session.prepare_background_wave(2);
    assert!(wave.prepared.iter().all(|(path, _)| path != &open));
    let (done, _) = session.commit_a_wave(wave, 2);
    // **The commit no longer stalls on that deferred file.** It used to break out of its loop when the head was an
    // open document the wave had deferred — and `prepare_wave` defers open documents, so the head was never popped
    // and the pump prepared the same wave for ever (`d344ba0e`: 17 rounds a second, one core, ~90 MB/s on
    // `containers.cpp`). `index_one` builds the summary itself, so the queue moves; what must not happen is the wave
    // *parsing* the open file's rendering, and the assertion above pins that.
    assert!(
        done.iter().any(|step| step.path == open),
        "the deferred open file is worked by the commit rather than left at the head: {done:?}"
    );
    assert!(
        session.index().summary(&open).is_some(),
        "so its summary exists: {done:?}"
    );
    assert!(
        session.index().summary(&other).is_some(),
        "and the other queued file makes progress"
    );

    let wave = session.prepare_background_wave(2);
    let (_done, _) = session.commit_a_wave(wave, 2);
    assert!(declares(&session, &open, "open_name"));
    assert!(declares(&session, &other, "other_name"));
}

#[test]
fn a_detached_include_read_preserves_the_project_paths_spelling() {
    let project = Project::new();
    let header = project.write("Header.h", "int declared_in_header;\n");
    let source = "#include \"Header.h\"\nint main() { return declared_in_header; }\n";
    let main = project.write("main.cpp", source);
    let mut session = project.session();
    session.did_open(&main, source);
    let read = session.gather_a_file_read(&main).unwrap().read();
    assert!(session.commit_a_file_read(read));
    let include = session
        .next_file_to_catch_up(&main)
        .expect("the include is queued");
    let read = session.gather_a_file_read(&include).unwrap().read();
    assert!(session.commit_a_file_read(read));
    assert_eq!(session.index().summary(&header).unwrap().path, header);
}

#[test]
fn a_previously_unreadable_include_can_be_requeued_for_a_detached_read() {
    let project = Project::new();
    let header = project.write("Header.h", "int declared_in_header;\n");
    let source = "#include \"Header.h\"\nint main() { return declared_in_header; }\n";
    let main = project.write("main.cpp", source);
    let mut session = project.session();
    session.did_open(&main, source);
    let main_read = session.gather_a_file_read(&main).unwrap().read();
    assert!(session.commit_a_file_read(main_read));

    std::fs::remove_file(&header).unwrap();
    session.advance(1);
    assert!(session.needs_catching_up(&main));
    assert!(session.next_file_to_catch_up(&main).is_none());
    std::fs::write(&header, "int declared_in_header;\n").unwrap();

    let include = session.ensure_file_to_catch_up(&main).unwrap();
    let read = session.gather_a_file_read(&include).unwrap().read();
    assert!(session.commit_a_file_read(read));
    assert!(declares(&session, &header, "declared_in_header"));
}

#[test]
fn local_completion_can_use_the_new_buffer_before_its_summary_is_ready() {
    let project = Project::new();
    let old = "struct Widget { int old_member; };\nvoid f() { Widget w; w.\n}\n";
    let new = "struct Widget { int new_member; };\nvoid f() { Widget w; w.\n}\n";
    let path = project.write("main.cpp", old);
    let mut session = project.session();
    session.did_open(&path, old);
    session.index_everything();
    session.did_change(&path, new);

    let view = session
        .view_of_the_file(&path)
        .expect("the edited buffer has a view");
    let offset = new.find("w.").unwrap() + 2;
    let completion = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(offset),
    );
    assert!(
        completion
            .items
            .iter()
            .any(|item| item.label == "new_member"),
        "the local member comes from the current buffer: {:?}",
        completion.items
    );
    assert!(
        !completion
            .items
            .iter()
            .any(|item| item.label == "old_member")
    );
}

#[test]
fn a_program_prepared_before_an_edit_is_discarded_and_remains_due() {
    let project = Project::new();
    let old = "#define OLD_MACRO 1\nint old_name;\n";
    let new = "#define NEW_MACRO 1\nint new_name;\n";
    let path = project.write("main.cpp", old);
    let mut session = project.session();
    session.did_open(&path, old);
    session.advance(1);
    let prepared = session
        .gather_a_unit_read(&path)
        .expect("gather the old program")
        .read_the_program()
        .expect("prepare the old program");

    session.did_change(&path, new);
    session.advance(1);
    assert_eq!(
        session.index().summary(&path).unwrap().key.content_hash,
        content_hash(new)
    );
    assert!(session.commit_a_program(&path, Some(prepared)).is_none());
    assert!(
        session
            .index()
            .cooked_declarations(&path)
            .is_none_or(|facts| facts.iter().all(|fact| fact.name != "old_name"))
    );
    assert!(
        session.the_wait_before_a_program_may_be_read().is_some(),
        "the edited program must remain eligible for a new read"
    );
}

#[test]
fn a_cook_prepared_before_another_document_changes_is_discarded() {
    let project = Project::new();
    let path = project.write("main.cpp", "int main_name;\n");
    let other = project.write("other.h", "#define VALUE 1\n");
    let mut session = project.session();
    session.did_open(&path, "int main_name;\n");
    session.index_everything();
    let materials = session.cooking_materials(&path).expect("cooking materials");
    let rendered = session.render_a_cooked(&materials).expect("rendering");

    session.did_change(&other, "#define VALUE 2\n");
    assert!(
        session.commit_a_cooked(rendered).is_none(),
        "a changed environment invalidates a cook even when its own text is unchanged"
    );
}

#[test]
fn a_detached_cook_cannot_publish_after_another_document_changes() {
    let project = Project::new();
    let path = project.write("main.cpp", "int main_name;\n");
    let other = project.write("other.h", "#define VALUE 1\n");
    let mut session = project.session();
    session.did_open(&path, "int main_name;\n");
    session.index_everything();
    let cook = session.gather_a_cook(&path).expect("detached materials");

    session.did_change(&other, "#define VALUE 2\n");
    let rendered = cook
        .render()
        .expect("the old materials can still be rendered");
    assert!(session.commit_a_cooked(rendered).is_none());
}

#[test]
fn a_stale_drain_keeps_its_refinement_request() {
    let project = Project::new();
    let path = project.write("main.cpp", "int old_name;\n");
    let mut session = project.session();
    session.add_project_files([path.clone()]);
    session.advance(1);
    session.want_refinement(&path);
    assert!(
        session.refinements_wanted() > 0,
        "the file needs refinement"
    );
    let wave = session.prepare_a_wave(1);
    let (_, drain) = session.commit_a_wave(wave, 1);
    assert!(!drain.is_empty(), "the fixture has refinement work");
    let prepared = session.prepare_the_drain(drain);

    session.did_change(&path, "int new_name;\n");
    assert_eq!(session.commit_the_drain(prepared), 0);
    assert!(
        session.refinements_wanted() > 0,
        "discarding stale work must not lose the request"
    );
}
