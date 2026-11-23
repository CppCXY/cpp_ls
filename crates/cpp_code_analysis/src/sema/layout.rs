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
//! # What is answered, and what is deliberately not
//!
//! **Bitfields** are placed by the Itanium/GCC x64 storage-unit rules — see [`ClassLayout::of_members`] for the
//! measured cases. **MSVC's bitfield ABI differs**, so the caller withholds a bitfield layout in that dialect rather
//! than applying gcc's rule to a cl.exe build.
//!
//! **`#pragma pack`** changes the rules by hand. **Virtual bases** add a vtable pointer that depends on the bases a
//! class has, which the fact does record (`bases`), but the size of a base is another class — that is handled, while a
//! vtable pointer is not added. A class that inherits therefore reports "no layout" rather than a plausible wrong one.

/// Where one data member sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLayout {
    pub name: String,
    /// Bytes from the start of the object — the **storage unit's** offset for a bitfield.
    pub offset: usize,
    /// The member's own size — the storage unit's size for a bitfield.
    pub size: usize,
    /// **Where the bits start**, for a bitfield: the absolute bit position in the object. `None` for a whole member.
    pub bit_offset: Option<usize>,
    /// **How many bits the declaration takes**, for a bitfield; `None` for a whole member.
    pub bit_width: Option<usize>,
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
    /// the result unusable — see [`ClassLayout::unknown`]. No member is a bitfield in this form; [`ClassLayout::of_members`]
    /// is the one that takes a width.
    pub fn of(members: &[(String, usize, usize)], union: bool, unknown: Vec<String>) -> Self {
        let members: Vec<(String, usize, usize, Option<usize>)> = members
            .iter()
            .map(|(name, size, alignment)| (name.clone(), *size, *alignment, None))
            .collect();
        Self::of_members(&members, union, unknown)
    }

    /// **The layout of a class whose members may be bitfields**, in declaration order.
    ///
    /// The bitfield rules are the Itanium/GCC x64 ones, measured on the compiler this project builds with:
    ///
    /// * A bitfield occupies a **storage unit** the size of its declared type, aligned to that type. A field starts
    ///   in the current unit only when the previous field had the *same* unit type and the bits still fit; otherwise
    ///   the next unit starts at the next multiple of the type's alignment.
    /// * `struct { unsigned _BitInt(8) a:3; unsigned _BitInt(8) b:5; unsigned _BitInt(8) c:3; }` is **2** bytes: `a`
    ///   and `b` share byte 0, `c` starts byte 1.
    /// * `struct { unsigned a:3; unsigned b:30; unsigned c:3; }` is **12** bytes: `b` does not fit the 29 remaining
    ///   bits of the first `unsigned` unit, and `c` does not fit the two remaining after it.
    /// * A **normal member** ends the current bitfield unit and starts at the next multiple of its own alignment;
    ///   the class's size is the bit cursor rounded up to its alignment.
    ///
    /// A zero-width bitfield asks for the next unit boundary and occupies no storage; it names no field.
    pub fn of_members(
        members: &[(String, usize, usize, Option<usize>)],
        union: bool,
        unknown: Vec<String>,
    ) -> Self {
        let mut fields = Vec::with_capacity(members.len());
        let mut alignment = 1usize;
        if union {
            let mut size = 0usize;
            for (name, member_size, member_alignment, bit_width) in members {
                alignment = alignment.max((*member_alignment).max(1));
                fields.push(FieldLayout {
                    name: name.clone(),
                    offset: 0,
                    size: *member_size,
                    bit_offset: bit_width.map(|_| 0),
                    bit_width: *bit_width,
                });
                size = size.max(*member_size);
            }
            return ClassLayout {
                size: size.max(1),
                alignment,
                union: true,
                fields,
                unknown,
            };
        }

        // The bit cursor, and the unit type of the bitfield immediately before it — the two facts the next field's
        // placement depends on.
        let mut bit_cursor = 0usize;
        let mut previous_unit: Option<(usize, usize)> = None;
        for (name, member_size, member_alignment, bit_width) in members {
            let member_size = *member_size;
            let member_alignment = (*member_alignment).max(1);
            alignment = alignment.max(member_alignment);
            let Some(width) = bit_width else {
                // A whole member: the next multiple of its alignment, and the bit cursor follows it.
                let offset = bit_cursor.div_ceil(member_alignment * 8) * member_alignment;
                fields.push(FieldLayout {
                    name: name.clone(),
                    offset,
                    size: member_size,
                    bit_offset: None,
                    bit_width: None,
                });
                bit_cursor = (offset + member_size) * 8;
                previous_unit = None;
                continue;
            };
            if *width == 0 {
                // A zero-width bitfield aligns the cursor to the type's unit and declares no field.
                bit_cursor = bit_cursor.div_ceil(member_alignment * 8) * member_alignment * 8;
                previous_unit = None;
                continue;
            }
            let unit_bits = member_size * 8;
            let shares = previous_unit == Some((member_size, member_alignment))
                && bit_cursor % unit_bits + *width <= unit_bits;
            let (unit_start, within) = if shares {
                let unit_start = bit_cursor.div_euclid(unit_bits) * unit_bits;
                (unit_start, bit_cursor - unit_start)
            } else {
                let start = bit_cursor.div_ceil(member_alignment * 8) * member_alignment * 8;
                (start, 0)
            };
            fields.push(FieldLayout {
                name: name.clone(),
                offset: unit_start / 8,
                size: member_size,
                bit_offset: Some(unit_start + within),
                bit_width: Some(*width),
            });
            bit_cursor = unit_start + within + *width;
            previous_unit = Some((member_size, member_alignment));
        }
        let size = bit_cursor.div_ceil(alignment * 8) * alignment;
        ClassLayout {
            size: size.max(1),
            alignment,
            union: false,
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
            let bits = match (field.bit_offset, field.bit_width) {
                (Some(at), Some(width)) => format!(" (bits {at}..{})", at + width),
                _ => String::new(),
            };
            out.push_str(&format!(
                "  {:<32} offset {:>3}, {} byte(s){bits}\n",
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
    for qualifier in [
        "const",
        "volatile",
        "static",
        "extern",
        "mutable",
        "thread_local",
    ] {
        if let Some(rest) = word.strip_prefix(qualifier)
            && rest.starts_with(char::is_whitespace)
        {
            word = rest.trim_start();
        }
    }
    // `unsigned long long`-style spellings are normalized by the caller's spelling, which may hold any of the words in
    // any order — so the answer is decided by the *set* of words that name a type.
    //
    // **C23's `_BitInt(N)`** first, because its size is a function of N and no word in the spelling says it: measured
    // with gcc 15 (MinGW) under `-std=gnu2x`, `_BitInt(1..8)` is 1 byte, `9..16` is 2, `17..32` is 4, `33..64` is 8,
    // `65..128` is 16, `129..192` is 24, `193..256` is 32 — powers of two up to 64 bits, then multiples of eight
    // bytes — and the alignment is the size up to eight, then eight.
    if let Some(bitint) = a_bitint_in_memory(word) {
        return Some(bitint);
    }
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
    // **MSVC's sized integer keywords**, which are how its own headers spell the standard typedefs:
    // `typedef __int64 ptrdiff_t;` in `vcruntime.h`, and `typedef unsigned __int64 size_t;` in `vadefs.h`. Without
    // these a C project on MSVC has no layout at all — `__int64` matched nothing and `unsigned __int64` matched the
    // `unsigned` branch below and came out **4 bytes**, which is a wrong number rather than a missing one.
    //
    // Measured on Lua 5.5.0's `lstate.h`: `StkIdRel` is `typedef union { StkId p; ptrdiff_t offset; } StkIdRel;`, its
    // `offset` is `ptrdiff_t` = `__int64`, and every `StkIdRel` member of `lua_State` was withheld — so the class
    // whose layout a reader most wants answered "not computed" naming six members.
    for (spelled, bytes) in [
        ("__int128", 16usize),
        ("__int64", 8),
        ("__int32", 4),
        ("__int16", 2),
        ("__int8", 1),
    ] {
        if has(spelled) {
            return Some((bytes, bytes.min(8)));
        }
    }
    if has("long") || has("int") || has("unsigned") || has("signed") {
        return Some((4, 4));
    }
    if has("void") {
        return Some((1, 1));
    }
    None
}

/// **The size and alignment of a `_BitInt(N)` spelling**, or `None` when the spelling is not one.
///
/// The measured rule is in [`a_builtin_in_memory`]'s note; this reads the width and applies it. Only the two
/// signedness words may stand beside it — `unsigned _BitInt(13)`, `_BitInt(13) signed` — so an unrelated spelling
/// that happens to contain the word is not sized as one.
fn a_bitint_in_memory(spelling: &str) -> Option<(usize, usize)> {
    let at = spelling.find("_BitInt")?;
    let before = spelling[..at].trim();
    if !matches!(before, "" | "unsigned" | "signed") {
        return None;
    }
    let rest = spelling[at + "_BitInt".len()..].trim_start();
    let rest = rest.strip_prefix('(')?.trim_start();
    let close = rest.find(')')?;
    let bits: usize = rest[..close].trim().parse().ok()?;
    let after = rest[close + 1..].trim();
    if !matches!(after, "" | "unsigned" | "signed") || bits == 0 {
        return None;
    }
    let bytes = bits.div_ceil(8);
    let size = if bits <= 64 {
        bytes.next_power_of_two()
    } else {
        bytes.div_ceil(8) * 8
    };
    Some((size, size.min(8)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measured with gcc 15 (MinGW) under `-std=gnu2x`: power-of-two sizes through 64 bits, then multiples of eight
    /// bytes; alignment is the size up to eight, and eight above that.
    #[test]
    fn a_bitint_is_sized_by_its_width() {
        for (bits, expected) in [
            (1usize, (1usize, 1usize)),
            (3, (1, 1)),
            (8, (1, 1)),
            (9, (2, 2)),
            (13, (2, 2)),
            (17, (4, 4)),
            (33, (8, 8)),
            (64, (8, 8)),
            (65, (16, 8)),
            (128, (16, 8)),
            (129, (24, 8)),
            (193, (32, 8)),
        ] {
            let spelling = format!("unsigned _BitInt({bits})");
            assert_eq!(a_builtin_in_memory(&spelling), Some(expected), "{spelling}");
        }
        assert_eq!(a_builtin_in_memory("_BitInt(13)"), Some((2, 2)));
        assert_eq!(a_builtin_in_memory("_BitInt(3) unsigned"), Some((1, 1)));
    }

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

    /// **MSVC's sized integer keywords are how its own headers spell the standard typedefs**, and a C project on
    /// MSVC had no layout without them: `typedef __int64 ptrdiff_t;` matched nothing, and `typedef unsigned __int64
    /// size_t;` matched the `unsigned` branch and came out **4 bytes** — wrong rather than missing.
    ///
    /// Measured on Lua 5.5.0's `lstate.h`: `StkIdRel` is `typedef union { StkId p; ptrdiff_t offset; } StkIdRel;`,
    /// and every `StkIdRel` member of `lua_State` was withheld.
    #[test]
    fn the_sized_integer_keywords_are_sized() {
        assert_eq!(a_builtin_in_memory("__int64"), Some((8, 8)));
        assert_eq!(a_builtin_in_memory("unsigned __int64"), Some((8, 8)));
        assert_eq!(a_builtin_in_memory("__int32"), Some((4, 4)));
        assert_eq!(a_builtin_in_memory("__int16"), Some((2, 2)));
        assert_eq!(a_builtin_in_memory("__int8"), Some((1, 1)));
        // The words around it still do not stop it, and a pointer to one is still a pointer.
        assert_eq!(a_builtin_in_memory("const __int64"), Some((8, 8)));
        assert_eq!(a_builtin_in_memory("__int64*"), Some((8, 8)));
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

    /// **A bare `unsigned` is an `unsigned int`** — the spelling `struct dim3 { unsigned x, y, z; }` writes, and the
    /// one whose absence made the whole struct unsizable. The combinations keep the sizes their own words decide.
    #[test]
    fn a_bare_unsigned_or_signed_is_an_int() {
        assert_eq!(a_builtin_in_memory("unsigned"), Some((4, 4)));
        assert_eq!(a_builtin_in_memory("signed"), Some((4, 4)));
        assert_eq!(a_builtin_in_memory("unsigned int"), Some((4, 4)));
        assert_eq!(a_builtin_in_memory("unsigned char"), Some((1, 1)));
        assert_eq!(a_builtin_in_memory("unsigned short"), Some((2, 2)));
        assert_eq!(a_builtin_in_memory("unsigned long"), Some((4, 4)));
        assert_eq!(a_builtin_in_memory("unsigned long long"), Some((8, 8)));
    }
}
