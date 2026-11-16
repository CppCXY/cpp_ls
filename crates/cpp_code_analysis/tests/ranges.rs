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

/// **A pipeline a reader names keeps the element the pipe yields.**
///
/// `element_of_a_range_expression` answers a pipe written in place, and `for (auto v : items | …)` has always worked.
/// The ordinary style binds the view first — `auto ids = items | …` — and every later use of `ids` asked its *type*,
/// which a view has no spelling for. Measured before this: the in-place loop was `Item` and the named one `auto`.
#[test]
fn a_named_pipeline_keeps_the_element_of_its_range() {
    let source = "
#include <ranges>
#include <vector>
struct Item { int id; };
int main() {
    std::vector<Item> items;
    auto ids = items | std::views::filter([](const Item& item) { return item.id > 0; });
    for (auto value : ids) {
        value.id = 0;
    }
}
";
    assert_eq!(
        type_after(source, "value :"),
        Some("Item".to_string()),
        "`ids` is the view the in-place pipe makes, so its element is `Item`"
    );
}

/// **A pipeline of two adaptors a reader names also keeps it** — the look-through is one variable, not one adaptor.
#[test]
fn a_named_pipeline_of_two_adaptors_keeps_it_too() {
    let source = "
#include <ranges>
#include <vector>
struct Item { int id; };
int main() {
    std::vector<Item> items;
    auto ids = items | std::views::filter([](const Item& item) { return item.id > 0; }) | std::views::take(2);
    for (auto value : ids) {
        value.id = 0;
    }
}
";
    assert_eq!(type_after(source, "value :"), Some("Item".to_string()));
}

/// **A generic lambda's parameter takes the element of the range it is handed.**
///
/// `[](auto value)` has no declaration that says what `value` is: the lambda's parameter is the placeholder, and the
/// adaptor call is the only thing that knows. Measured before this: `value` was `auto`/unknown, so `value.id` resolved
/// nothing and completion inside the lambda offered nothing.
#[test]
fn a_generic_lambdas_parameter_takes_the_ranges_element() {
    let source = "
#include <ranges>
#include <vector>
struct Item { int id; };
int main() {
    std::vector<Item> items;
    auto ids = items | std::views::filter([](auto value) { return value.id > 0; });
    for (auto item : ids) {
        item.id = 0;
    }
}
";
    assert_eq!(
        type_after(source, "value.id"),
        Some("Item".to_string()),
        "`value` receives what `items` yields"
    );
}

/// **And the parameter's type is what makes a transform's element answerable** — `[](auto n)` is `int` over
/// `iota(1, 6)`, so `n * n` is an `int` and the loop variable is one too.
#[test]
fn a_generic_lambdas_parameter_feeds_the_transform_element() {
    let source = "
#include <ranges>
int main() {
    for (auto value : std::views::iota(1, 6) | std::views::transform([](auto n) { return n * n; })) {
        value = 0;
    }
}
";
    assert_eq!(type_after(source, "value :"), Some("int".to_string()));
}

/// **The parameter's reference shape is the declaration's, not the element's** — `auto&` over a container's element
/// is a `T&`, which is what the lambda body writes through.
#[test]
fn a_generic_lambdas_parameter_keeps_the_reference_it_wrote() {
    let source = "
#include <ranges>
#include <vector>
struct Item { int id; };
int main() {
    std::vector<Item> items;
    auto ids = items | std::views::filter([](auto& value) { return value.id > 0; });
    for (auto item : ids) {
        item.id = 0;
    }
}
";
    assert_eq!(type_after(source, "value.id"), Some("Item&".to_string()));
}

/// **A lambda handed to something else keeps the placeholder.** The table is the standard adaptors and algorithms
/// only: a project's own `apply(f)` says nothing about what `f` receives, and a guess there would type the body
/// against the wrong class. Measured before the table existed, and re-pinned here so a later widening has to say so.
#[test]
fn a_lambda_outside_the_standard_adaptors_keeps_its_placeholder() {
    let source = "
#include <vector>
struct Item { int id; };
struct applier { template <class F> void operator()(F f) const; };
void use(std::vector<Item> items) {
    applier{}([](auto value) { value.id = 0; });
}
";
    assert_eq!(type_after(source, "value.id"), None);
}

/// **The algorithm form writes the range first** — `std::ranges::find_if(items, f)` — and the element is the
/// first argument's, not a pipe's.
#[test]
fn a_generic_lambdas_parameter_in_an_algorithm_takes_the_first_arguments_element() {
    let source = "
namespace std {
template <class T> struct vector { };
namespace ranges { void find_if(); }
}
struct Item { int id; };
void use(std::vector<Item> items) {
    std::ranges::find_if(items, [](auto value) { value.id = 0; });
}
";
    assert_eq!(type_after(source, "value.id"), Some("Item".to_string()));
}

/// **A transform that projects a member answers that member's type** — `[](auto item) { return item.name; }`.
///
/// The parameter is `Item` now (3b), so the missing half was the *return*: a member access needs the class's member
/// list, which the syntax-only deduction does not have. Measured before this: the loop over `names` answered `auto`.
#[test]
fn a_transform_of_a_generic_lambdas_member_answers_the_members_type() {
    let source = "
#include <ranges>
#include <string>
#include <vector>
struct Item { int id; std::string name; };
int main() {
    std::vector<Item> items;
    auto names = items | std::views::transform([](auto item) { return item.name; });
    for (auto name : names) {
        name.size();
    }
}
";
    assert_eq!(
        type_after(source, "name :"),
        Some("std::string".to_string()),
        "`names` yields what `item.name` is"
    );
}

/// **The parameter does not have to be generic** — `[](const Item& item)` names its own type, and the member lookup is
/// the same question.
#[test]
fn a_transform_of_a_written_parameters_member_answers_the_members_type() {
    let source = "
#include <ranges>
#include <string>
#include <vector>
struct Item { int id; std::string name; };
int main() {
    std::vector<Item> items;
    auto names = items | std::views::transform([](const Item& item) { return item.name; });
    for (auto name : names) {
        name.size();
    }
}
";
    assert_eq!(
        type_after(source, "name :"),
        Some("std::string".to_string())
    );
}

/// **A member the class does not have stays refused** — the answer is the class's own list, not a guess from a name.
#[test]
fn a_transform_of_a_member_that_is_not_declared_stays_unknown() {
    let source = "
#include <ranges>
#include <vector>
struct Item { int id; };
int main() {
    std::vector<Item> items;
    auto names = items | std::views::transform([](auto item) { return item.missing; });
    for (auto name : names) {
        name = 0;
    }
}
";
    assert_eq!(type_after(source, "name :"), None);
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
