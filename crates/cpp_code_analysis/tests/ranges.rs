//! **The element a range yields** — what a `for` binds, through C++20's views and algorithms.
//!
//! A view's own type is unnamed (a class template over a closure type), so nothing here tries to spell it: the question is
//! the *element*, which is what a reader writes next (`value.member`, `it->name`) and what completion needs. The shapes are
//! `ranges.cpp`'s: a pipe chain, `views::iota`, `views::keys`/`values`, `views::join`, and the `std::ranges` algorithms
//! whose answers the standard fixes.

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// A session over one file, cooked and indexed, so a type question reads the same tree the server would.
fn session_with(source: &str) -> (Session<MemoryFiles>, cpp_code_analysis::FileView) {
    let memory = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");
    let view = session
        .view_of_the_file(std::path::Path::new("/p/a.cpp"))
        .expect("the file is held");
    (session, view)
}

/// The type of the variable whose name follows `needle`, as the engine spells it.
fn type_after(source: &str, needle: &str) -> Option<String> {
    let (session, view) = session_with(source);
    let at = source.find(needle).expect("the fixture writes it");
    session
        .type_at(
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
        )
        .value()
        .map(|found| found.type_of.clone())
}

/// **A pipe chain keeps the element of the range it starts from.**
#[test]
fn a_pipe_chain_keeps_the_element_of_its_left_operand() {
    let source = "
#include <ranges>
#include <vector>
int main() {
    std::vector<int> numbers{5, 3, 1};
    for (auto value : numbers | std::views::filter([](int v) { return v > 2; }) | std::views::take(2)) {
        value = 0;
    }
}
";
    assert_eq!(
        type_after(source, "value :"),
        Some("int".to_string()),
        "`numbers` yields `int`, and filter/take do not change it"
    );
}

/// **`views::iota` yields the type of its first argument.**
#[test]
fn a_counted_range_yields_its_bound() {
    let source = "
#include <ranges>
int main() {
    for (auto value : std::views::iota(1, 6)) {
        value = 0;
    }
}
";
    assert_eq!(type_after(source, "value :"), Some("int".to_string()));
}

/// **`keys` and `values` are the two halves of a map's element.**
#[test]
fn the_key_of_a_map_is_a_view_of_the_key() {
    let source = "
#include <map>
#include <ranges>
#include <string>
int main() {
    std::map<std::string, int> map;
    for (auto key : map | std::views::keys) {
        key = 0;
    }
    for (auto value : map | std::views::values) {
        value = 0;
    }
}
";
    assert_eq!(
        type_after(source, "key :"),
        Some("std::string".to_string()),
        "the key, not `std::pair<const std::string, int>`"
    );
    assert_eq!(type_after(source, "value :"), Some("int".to_string()));
}

/// **`join` flattens one level.**
#[test]
fn a_joined_range_yields_the_inner_element() {
    let source = "
#include <ranges>
#include <vector>
int main() {
    std::vector<std::vector<int>> rows;
    for (auto value : rows | std::views::join) {
        value = 0;
    }
}
";
    assert_eq!(type_after(source, "value :"), Some("int".to_string()));
}

/// **`std::ranges::find` answers the element it found** — what a reader dereferences.
#[test]
fn a_ranges_algorithm_answers_the_element() {
    let source = "
#include <ranges>
#include <vector>
struct Person { int age; };
int main() {
    std::vector<Person> people;
    auto found = std::ranges::find(people, Person{}, &Person::age);
}
";
    assert_eq!(
        type_after(source, "found ="),
        Some("Person".to_string()),
        "an iterator to the element is, to a reader, the element"
    );
}

/// **`std::ranges::size` and `distance` are counts.**
#[test]
fn the_counting_algorithms_are_ptrdiff_t() {
    let source = "
#include <ranges>
#include <vector>
int main() {
    std::vector<int> numbers;
    auto count = std::ranges::size(numbers);
    auto from = std::ranges::distance(numbers);
}
";
    assert_eq!(
        type_after(source, "count ="),
        Some("std::size_t".to_string())
    );
    assert_eq!(
        type_after(source, "from ="),
        Some("std::ptrdiff_t".to_string())
    );
}

/// **`views::transform`'s element is the lambda's return type** — the deduction `a_lambdas_return_type` already had, so
/// this is one answer rather than a guess.
#[test]
fn a_transform_answers_the_lambdas_return() {
    let source = "
#include <ranges>
#include <vector>
int main() {
    std::vector<int> numbers;
    for (auto value : numbers | std::views::transform([](int v) { return v * 10; })) {
        value = 0;
    }
}
";
    assert_eq!(
        type_after(source, "value :"),
        Some("int".to_string()),
        "`v * 10` is an `int`, and that is what the transform yields"
    );
}

/// **And a body this layer cannot type is still refused** — a transform that answers a wrong element would offer the loop
/// wrong members, which is the failure the refusal exists for.
#[test]
fn a_transform_of_an_unclearable_body_is_refused() {
    let source = "
#include <ranges>
#include <vector>
struct S { int field; };
void f(S other) {
    std::vector<int> numbers;
    for (auto value : numbers | std::views::transform([&](int v) { return other; })) {
        value = 0;
    }
}
";
    // The body returns a parameter of a **captured** object — a name this syntax walk cannot type — so the element is
    // unknown rather than `S`.
    assert_eq!(type_after(source, "value :"), None);
}
