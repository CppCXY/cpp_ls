//! **The rule names a `--code` filter may use** — the enum's own list, so a filter is a name the server also knows.
//!
//! `codes` resolves a filter once, at the start of a run: an unknown name is a typo, and a run that silently reports
//! nothing because of one is worse than one that refuses.

use std::collections::BTreeSet;
/// **The rule names a filter may use**, which is the enum's own list — a filter is a name the server also knows.
pub fn rule_names() -> Vec<&'static str> {
    cpp_code_analysis::DiagnosticCode::ALL
        .iter()
        .map(|code| code.name())
        .collect()
}

/// **The codes a filter names**, resolved against the enum — so `--code=undefined-nam` is a message rather than a run
/// that reports nothing. `Err` is the name nobody has.
pub fn codes(named: &[String]) -> Result<Option<BTreeSet<&'static str>>, String> {
    if named.is_empty() {
        return Ok(None);
    }
    let all = rule_names();
    let mut resolved = BTreeSet::new();
    for name in named {
        match all.iter().find(|known| *known == name) {
            Some(known) => {
                resolved.insert(*known);
            }
            None => return Err(name.clone()),
        }
    }
    Ok(Some(resolved))
}
