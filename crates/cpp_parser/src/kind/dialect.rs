/// Which **compiler's own spellings** this parse should recognise.
///
/// The standard reserves the double-underscore names to the implementation, and the implementations give the same
/// spelling different meanings. That is the whole reason this exists — for a handful of names the answer to "is
/// this a type" is not in the language at all:
///
/// ```text
///                                 GNU (g++, clang++)              MSVC (cl.exe)
/// __int128                        a builtin type                  not a type at all
/// __int64                         not a type (`_mingw.h` #defines  a builtin type
///                                 it to `long long`)
/// _Float16, __bf16, __float128     builtin types                  not spelled at all
/// ```
///
/// So a parser that answers "is `__int128` a type" the same way for every target is wrong for one of them, and the
/// wrong answer is not a diagnostic — it is a *silent wrong tree*: with `__int128` read as a name,
/// `unsigned __int128 x;` comes out as a declaration of a variable called `__int128` with a macro suffix `x`
/// (`bits/bmi2intrin.h`, measured).
///
/// # Why this is not [`CppLanguageLevel`](crate::kind::CppLanguageLevel)
///
/// A dialect and a standard level are different questions, and the level's own vocabulary shows the seam: it has
/// `GnuCpp` and `MsvcCpp` variants, which are *levels*, so a project compiled with `-std=gnu++20` cannot be
/// described — it is C++20 **and** GNU's spellings at once. The two are independent here: `level` gates features
/// (raw strings, `<=>`), `dialect` decides what the compiler's reserved names mean.
///
/// # Where the answer comes from
///
/// The compiler itself, not a guess: GCC and Clang predefine `__GNUC__`, MSVC predefines `_MSC_VER`, and the macro
/// table the analysis layer already asks for with `-dM -E` carries both (see `CompilerConfig::dialect`). The
/// default is [`Dialect::Gnu`] because that is what the parser's own corpus is compiled by, and because the
/// ambiguity is not symmetric: reading `__int128` as a name is a silent wrong tree, while reading `__int64` as a
/// name is merely conservative (the `#define` that would have made it one is a directive, and a directive is not
/// this layer's business).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dialect {
    /// GCC and Clang: `__int128`, `_Float16`, `__bf16` and friends are types.
    #[default]
    Gnu,

    /// MSVC: `__int8`, `__int16`, `__int32`, `__int64` are types, and `__int128` is not one.
    Msvc,
}

impl Dialect {
    /// The dialect a compiler's **predefined macros** say it is.
    ///
    /// `None` when the table has neither marker, which is "nobody said", not "neither" — the caller keeps
    /// whatever it had. Both names are the compiler's own statement about itself: `__GNUC__` is defined by GCC and
    /// by Clang (which is why Clang is on this side), `_MSC_VER` only by MSVC and its compatible drivers.
    ///
    /// Only the *names* are read, so the caller may pass whatever its macro table holds — a `(name, value)` pair
    /// for each entry, with any spelling of the two strings. That is the shape `Toolchain::macros` already
    /// answers with, which is what keeps this function on the parser side of the boundary.
    pub fn from_predefined_macros<I, N, V>(macros: I) -> Option<Self>
    where
        I: IntoIterator<Item = (N, V)>,
        N: AsRef<str>,
    {
        let mut gnu = false;
        let mut msvc = false;

        for (name, _) in macros {
            match name.as_ref() {
                "__GNUC__" => gnu = true,
                "_MSC_VER" => msvc = true,
                _ => {}
            }
        }

        match (gnu, msvc) {
            // A compiler that defines both is claiming GNU compatibility on top of MSVC (`clang-cl`), and its
            // *spellings* are MSVC's: that is the half this decides.
            (_, true) => Some(Dialect::Msvc),
            (true, false) => Some(Dialect::Gnu),
            (false, false) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Dialect;

    /// The macros are the compiler's own statement about itself, and the two markers are what it says it with.
    #[test]
    fn the_predefined_macros_say_which_compiler_it_is() {
        let gnu = [("__GNUC__", Some("15")), ("__cplusplus", Some("202002L"))];
        let clang = [("__clang__", Some("1")), ("__GNUC__", Some("4"))];
        let msvc = [("_MSC_VER", Some("1941")), ("_MSC_FULL_VER", Some("194134120"))];
        // `clang-cl` defines both: it is MSVC's *language* on Clang's back end, so its spellings are MSVC's.
        let clang_cl = [("__clang__", Some("1")), ("_MSC_VER", Some("1941"))];

        assert_eq!(Dialect::from_predefined_macros(gnu), Some(Dialect::Gnu));
        assert_eq!(Dialect::from_predefined_macros(clang), Some(Dialect::Gnu));
        assert_eq!(Dialect::from_predefined_macros(msvc), Some(Dialect::Msvc));
        assert_eq!(Dialect::from_predefined_macros(clang_cl), Some(Dialect::Msvc));

        // Nothing to go on is **not** an answer: the caller keeps what it had rather than being told "neither".
        let unknown = [("__STDC__", Some("1"))];
        assert_eq!(Dialect::from_predefined_macros(unknown), None);
        assert_eq!(
            Dialect::from_predefined_macros(Vec::<(&str, Option<&str>)>::new()),
            None
        );
    }
}
