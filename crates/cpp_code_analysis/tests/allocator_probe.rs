//! **The allocator chain, isolated** — what `std::vector<T>::reference` needs, in a fixture with no standard library.
//!
//! The declarations below are libstdc++'s shapes with the names changed: `mine::allocator` is `std::allocator`,
//! `mine::allocator_traits` is `std::allocator_traits` (including its **alias template** `rebind_alloc`),
//! `mygnu::alloc_traits` is `__gnu_cxx::__alloc_traits` (a base and a `value_type&` alias), and `mygnu::vector_base`
//! / `mygnu::vector` are `_Vector_base` and `vector`.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test allocator_probe -- --nocapture
//! ```

use cpp_code_analysis::{CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn session(source: &str) -> Session<MemoryFiles> {
    let providers = SessionFiles::new(
        OpenDocuments::new(),
        MemoryFiles::new().with_file("/p/a.cpp", source),
    );
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    session
}

fn asked(source: &str, needle: &str, skip: usize) -> String {
    let session = session(source);
    let view = session.view("/p/a.cpp").expect("cooked");
    let at = source.find(needle).expect("the fixture writes it") + skip;
    let reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(at))
        .expect("in the file");
    match session.type_at(&view, reading) {
        cpp_code_analysis::Known::Yes(found) => format!("{} [{}]", found.type_of, found.expression),
        cpp_code_analysis::Known::No => "no".to_string(),
        cpp_code_analysis::Known::Unknown(reason) => format!("unknown({reason:?})"),
    }
}

const CHAIN: &str = "\
namespace mine {
template <typename Tp>
class allocator {
public:
    typedef Tp value_type;
    template <typename Tp1>
    struct rebind { typedef allocator<Tp1> other; };
};
template <typename Alloc>
struct allocator_traits {
    typedef typename Alloc::value_type value_type;
    template <typename Up>
    using rebind_alloc = typename Alloc::template rebind<Up>::other;
};
}
namespace mygnu {
template <typename Alloc>
struct alloc_traits : mine::allocator_traits<Alloc> {
    typedef mine::allocator_traits<Alloc> base_type;
    typedef typename base_type::value_type value_type;
    typedef value_type& reference;
    template <typename Tp>
    struct rebind { typedef typename base_type::template rebind_alloc<Tp> other; };
};
template <typename Tp, typename Alloc>
struct vector_base {
    typedef typename mygnu::alloc_traits<Alloc>::template rebind<Tp>::other tp_alloc_type;
};
template <typename Tp, typename Alloc>
class vector : protected vector_base<Tp, Alloc> {
    typedef vector_base<Tp, Alloc> base;
    typedef typename base::tp_alloc_type tp_alloc_type;
    typedef mygnu::alloc_traits<tp_alloc_type> alloc_traits_type;
public:
    typedef typename alloc_traits_type::reference reference;
    reference at(int i);
    reference operator[](int i);
};
}
void f() {
    mygnu::vector<int, mine::allocator<int>> v;
    int x = v[0];
    int y = v.at(1);
    (void)x;
    (void)y;
}
";

#[test]
fn what_the_containers_alias_chain_answers() {
    println!("v[0]  → {}", asked(CHAIN, "v[0]", 2));
    println!("v.at  → {}", asked(CHAIN, "v.at(1)", 2));
}
