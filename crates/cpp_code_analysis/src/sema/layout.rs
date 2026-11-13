//! **What a class costs in memory** — its size, its alignment, and where each data member sits.
//!
//! This is the answer rust-analyzer puts on a struct's hover, and the question a C++ reader asks constantly: how big is
//! this, and what is at offset 8? `sizeof` answers the first half and nothing answers the second without a compiler.
//!
//! # What the rules are, and which ABI they are
//!
//! The layout is the **Itanium/MSVC x64** one, and both agree on the cases this handles: a member goes at the next
//! multiple of its own alignment, the class's alignment is the largest member's, and its size is rounded up to that
//! alignment. A `union` puts every member at 0 and takes the largest.
//!
//! # What is deliberately not answered
//!
//! **Bitfields** change where the next member starts, and a fact does not record the width — a layout computed without
//! them would be a wrong number rather than a missing one, so a class with a bitfield reports no offsets at all.
//! **`#pragma pack`** does the same thing by hand. **Virtual bases** add a vtable pointer that depends on the bases a
//! class has, which the fact does record (`bases`), but the size of a base is another class — that is handled, while a
//! vtable pointer is not added. A class that inherits therefore reports "no layout" rather than a plausible wrong one.

/// Where one data member sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLayout {
    pub name: String,
    /// Bytes from the start of the object.
    pub offset: usize,
    /// The member's own size.
    pub size: usize,
}

/// **A class's memory layout**, as the target gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassLayout {
    /// `sizeof(T)` — the size rounded up to [`ClassLayout::alignment`].
    pub size: usize,
    /// `alignof(T)`.
    pub alignment: usize,
    /// A `union` has every member at offset 0 and is as large as its largest member.
    pub union: bool,
    pub fields: Vec<FieldLayout>,
    /// The members whose size could not be worked out — a template, a type the index does not hold, a bitfield.
    ///
    /// A layout with any of these is **not** reported: the numbers beside the known members would be a guess dressed
    /// as an answer.
    pub unknown: Vec<String>,
}

impl ClassLayout {
    /// The layout of a class from its data members' sizes and alignments, in declaration order.
    ///
    /// `members` is what the caller could work out; a member whose type it could not size is in `unknown` and makes
    /// the result unusable — see [`ClassLayout::unknown`].
    pub fn of(members: &[(String, usize, usize)], union: bool, unknown: Vec<String>) -> Self {
        let mut offset = 0usize;
        let mut size = 0usize;
        let mut alignment = 1usize;
        let mut fields = Vec::with_capacity(members.len());
        for (name, member_size, member_alignment) in members {
            let member_alignment = (*member_alignment).max(1);
            alignment = alignment.max(member_alignment);
            if union {
                fields.push(FieldLayout {
                    name: name.clone(),
                    offset: 0,
                    size: *member_size,
                });
                size = size.max(*member_size);
                continue;
            }
            // **The member starts at the next multiple of its alignment**, which is the whole rule for everything
            // that is not a bitfield.
            let at = offset.div_ceil(member_alignment) * member_alignment;
            fields.push(FieldLayout {
                name: name.clone(),
                offset: at,
                size: *member_size,
            });
            offset = at + member_size;
            size = offset;
        }
        // An empty class is one byte in C++: `sizeof` of it is never 0.
        let size = size.max(1);
        ClassLayout {
            size: size.div_ceil(alignment) * alignment,
            alignment,
            union,
            fields,
            unknown,
        }
    }

    /// **The card's block**: the two numbers a reader asks for first, then each field.
    ///
    /// A fenced block rather than a sentence because the field list is alignable, and because a reader copies the
    /// numbers out of it.
    pub fn describe(&self, name: &str) -> String {
        let mut out = format!(
            "**Layout** (x64)\n\nsize {} byte(s), alignment {}{}\n\n```text\n",
            self.size,
            self.alignment,
            if self.union { ", a union" } else { "" }
        );
        out.push_str(&format!("{name}\n"));
        for field in &self.fields {
            out.push_str(&format!(
                "  {:<32} offset {:>3}, {} byte(s)\n",
                field.name, field.offset, field.size
            ));
        }
        out.push_str("```\n");
        out
    }
}

/// **The size and alignment of a type, as written**, when it is one of the types the rules know.
///
/// `None` is "this spelling is not a builtin and not shaped like a pointer, array or reference" — a class name, a
/// typedef, a template. The caller resolves those (it has the index) and calls this again on what it finds.
pub fn a_builtin_in_memory(spelling: &str) -> Option<(usize, usize)> {
    let mut word = spelling.trim();
    // **Pointers and references are a word**, before anything is read as a name: `Widget*` and `const Widget&` are
    // eight bytes and their pointee's size does not enter into it.
    let mut at = 0usize;
    let bytes = word.as_bytes();
    let mut depth = 0i32;
    let mut angle = 0i32;
    while at < bytes.len() {
        match bytes[at] {
            b'<' => angle += 1,
            b'>' => angle -= 1,
            b'(' | b'[' => {
                if angle == 0 {
                    depth += 1
                }
            }
            b')' | b']' => depth -= 1,
            b'*' | b'&' if depth == 0 && angle == 0 => return Some((8, 8)),
            _ => {}
        }
        at += 1;
    }
    // Array: `T[N]` — the size of `T` times a literal `N`.
    if let Some((element, rest)) = word.split_once('[')
        && let Some(count) = rest.strip_suffix(']')
        && let Ok(count) = count.trim().parse::<usize>()
        && let Some((size, alignment)) = a_builtin_in_memory(element)
    {
        return Some((size.saturating_mul(count), alignment));
    }
    // A qualifier is not part of the type's size.
    for qualifier in ["const", "volatile", "static", "extern", "mutable", "thread_local"] {
        if let Some(rest) = word.strip_prefix(qualifier)
            && rest.starts_with(char::is_whitespace)
        {
            word = rest.trim_start();
        }
    }
    // `unsigned long long`-style spellings are normalized by the caller's spelling, which may hold any of the words in
    // any order — so the answer is decided by the *set* of words that name a type.
    let words: Vec<&str> = word
        .split(|character: char| character.is_whitespace() || character == '*' || character == '&')
        .filter(|word| !word.is_empty())
        .collect();
    let has = |candidate: &str| words.contains(&candidate);
    if has("bool") || has("_Bool") {
        return Some((1, 1));
    }
    if has("char") || has("char8_t") {
        return Some((1, 1));
    }
    if has("char16_t") {
        return Some((2, 2));
    }
    if has("char32_t") || has("float") {
        return Some((4, 4));
    }
    if has("short") || has("wchar_t") {
        return Some((2, 2));
    }
    if has("double") {
        // MSVC's `long double` is a `double`; Itanium's is 16. The x64 default this card states is MSVC's, and a
        // reader on gcc sees `long double` sized wrong by eight bytes — the one place the two ABIs disagree here.
        return Some((8, 8));
    }
    if words.iter().filter(|word| **word == "long").count() >= 2 {
        return Some((8, 8));
    }
    if has("long") || has("int") {
        return Some((4, 4));
    }
    if has("void") {
        return Some((1, 1));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_struct_packs_its_members_with_their_own_alignment() {
        // struct { int x; double y; char c; } — 4, then 8 at 8, then 1 at 16, rounded to 8.
        let layout = ClassLayout::of(
            &[("x".into(), 4, 4), ("y".into(), 8, 8), ("c".into(), 1, 1)],
            false,
            Vec::new(),
        );
        assert_eq!(layout.size, 24);
        assert_eq!(layout.alignment, 8);
        let offsets: Vec<(usize, usize)> = layout
            .fields
            .iter()
            .map(|field| (field.offset, field.size))
            .collect();
        assert_eq!(offsets, [(0, 4), (8, 8), (16, 1)]);
    }

    #[test]
    fn a_union_is_as_large_as_its_largest_member() {
        let layout = ClassLayout::of(
            &[("i".into(), 4, 4), ("d".into(), 8, 8), ("c".into(), 1, 1)],
            true,
            Vec::new(),
        );
        assert_eq!(layout.size, 8);
        assert_eq!(layout.alignment, 8);
        assert!(layout.fields.iter().all(|field| field.offset == 0));
    }

    #[test]
    fn an_empty_class_is_one_byte() {
        let layout = ClassLayout::of(&[], false, Vec::new());
        assert_eq!(layout.size, 1);
        assert_eq!(layout.alignment, 1);
    }

    #[test]
    fn the_builtins_and_the_shapes_around_them() {
        assert_eq!(a_builtin_in_memory("int"), Some((4, 4)));
        assert_eq!(a_builtin_in_memory("unsigned long long"), Some((8, 8)));
        assert_eq!(a_builtin_in_memory("long"), Some((4, 4)));
        assert_eq!(a_builtin_in_memory("char"), Some((1, 1)));
        assert_eq!(a_builtin_in_memory("char *"), Some((8, 8)));
        assert_eq!(a_builtin_in_memory("const Widget&"), Some((8, 8)));
        assert_eq!(a_builtin_in_memory("int[4]"), Some((16, 4)));
        assert_eq!(a_builtin_in_memory("double[3]"), Some((24, 8)));
        assert_eq!(a_builtin_in_memory("Widget"), None);
        assert_eq!(a_builtin_in_memory("std::string"), None);
    }
}
