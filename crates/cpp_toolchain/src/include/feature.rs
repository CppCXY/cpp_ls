//! **The `__has_*` operators the preprocessor cannot answer from a macro table.**
//!
//! `__has_builtin(__builtin_expect)`, `__has_feature(cxx_lambdas)`, `__has_attribute(unused)` and their siblings are
//! **compiler questions**, not macro questions: the compiler knows the answer, and no table this analysis keeps can
//! be trusted to agree with it. Folded into `false` they grey out code the compiler compiles; folded into `true` they
//! grey out the other half. The condition evaluator already refuses both ([`crate::Value::Unknown`]); what it needs is
//! somebody who can ask.
//!
//! This module is that somebody. [`CompilerFeatureProbe`] writes the question as a two-line translation unit —
//!
//! ```c
//! #if __has_builtin(__builtin_expect)
//! CPPLS_PROBE_YES
//! #else
//! CPPLS_PROBE_NO
//! #endif
//! ```
//!
//! — and runs the project's own compiler over it. The answer is the compiler's, in the language and standard the
//! file is built as; a compiler that does not have the operator says so with a diagnostic, and that is `None`
//! ("nobody can answer") rather than a false answer.
//!
//! # Why the answer is cached, and what the cache is keyed by
//!
//! A header can write the same `__has_builtin` in twenty places, and a process per question would make opening a
//! project slower than the parse it feeds. The cache is keyed by the operator, its operand, the language and the
//! standard, because those are everything the compiler's answer depends on; the compiler is the probe's own field.
//! It is shared by the session's probes ([`FeatureCache`]), so the second file that asks costs a map lookup.
//!
//! # What is deliberately not answered here
//!
//! `__has_include` and `__has_include_next` are about **files**, and this module has no search path — they are
//! answered by the include resolver, as they always were. `__has_cpp_attribute` is answered from the table in
//! [`crate::cooked`] for the compilers that table has been measured against. This module is the remaining family.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::toolchain::{CommandRunner, Output};

/// **Which language the compiler is asked in** — the `-x` flag, and the probe file's extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeLanguage {
    C,
    Cpp,
}

impl ProbeLanguage {
    fn flag(self) -> &'static str {
        match self {
            ProbeLanguage::C => "c",
            ProbeLanguage::Cpp => "c++",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            ProbeLanguage::C => "c",
            ProbeLanguage::Cpp => "cpp",
        }
    }
}

/// One `__has_*` question, answered by whatever the implementing probe considers authoritative.
///
/// `None` is **not** `0`: it means this probe could not ask, and the condition evaluator turns it into
/// [`crate::Value::Unknown`] — the answer that keeps a branch from being read the wrong way.
pub trait FeatureProbe: Send + Sync {
    fn answer(&self, operator: &str, operand: &str) -> Option<i128>;
}

/// **The answers this session has already been given**, shared by every file's probe.
///
/// A newtype rather than the bare `Arc<Mutex<…>>` so the key stays in one place: three fields that are all part of
/// what the compiler's answer depends on, and none of which is the compiler itself — the probe owns that.
#[derive(Clone, Default)]
pub struct FeatureCache {
    answers: Arc<
        std::sync::Mutex<HashMap<(String, String, ProbeLanguage, Option<String>), Option<i128>>>,
    >,
}

impl FeatureCache {
    fn get(
        &self,
        operator: &str,
        operand: &str,
        language: ProbeLanguage,
        standard: Option<&str>,
    ) -> Option<Option<i128>> {
        let key = (
            operator.to_string(),
            operand.to_string(),
            language,
            standard.map(str::to_string),
        );
        self.answers
            .lock()
            .ok()
            .and_then(|answers| answers.get(&key).copied())
    }

    fn put(
        &self,
        operator: &str,
        operand: &str,
        language: ProbeLanguage,
        standard: Option<&str>,
        value: Option<i128>,
    ) {
        let key = (
            operator.to_string(),
            operand.to_string(),
            language,
            standard.map(str::to_string),
        );
        if let Ok(mut answers) = self.answers.lock() {
            answers.insert(key, value);
        }
    }
}

/// **A `__has_*` operator answered by running the project's compiler.**
pub struct CompilerFeatureProbe {
    program: PathBuf,
    runner: Arc<dyn CommandRunner + Send + Sync>,
    language: ProbeLanguage,
    /// The `-std=` the compiler is asked with — `gnu++23`, `c11`. `None` asks its default, which is what a project
    /// that stated no standard is built as.
    standard: Option<String>,
    cache: FeatureCache,
}

impl CompilerFeatureProbe {
    pub fn new(
        program: impl Into<PathBuf>,
        runner: Arc<dyn CommandRunner + Send + Sync>,
        language: ProbeLanguage,
        standard: Option<String>,
    ) -> Self {
        Self {
            program: program.into(),
            runner,
            language,
            standard,
            cache: FeatureCache::default(),
        }
    }

    /// The same probe, sharing a session's answers rather than starting a cache of its own.
    pub fn with_cache(mut self, cache: FeatureCache) -> Self {
        self.cache = cache;
        self
    }

    /// The operators this probe answers. Anything else is `None` — the caller keeps its own rule.
    fn is_a_question_this_probe_answers(operator: &str) -> bool {
        matches!(
            operator,
            "__has_builtin"
                | "__has_feature"
                | "__has_extension"
                | "__has_attribute"
                | "__has_declspec_attribute"
        )
    }

    /// **The operand is pasted into a source file**, so only the characters a name, a feature or an attribute can
    /// contain are accepted. A refusal is `None`, not a sanitised question: an operand with a quote or a newline in
    /// it is not a question this probe knows how to ask.
    fn is_a_plausible_operand(operand: &str) -> bool {
        !operand.is_empty()
            && operand.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '_' || character == ':'
            })
    }

    fn ask(&self, operator: &str, operand: &str) -> Option<i128> {
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let file = std::env::temp_dir().join(format!(
            "cppls-feature-{}-{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            self.language.extension(),
        ));
        let source =
            format!("#if {operator}({operand})\nCPPLS_PROBE_YES\n#else\nCPPLS_PROBE_NO\n#endif\n");
        std::fs::write(&file, source).ok()?;

        let mut arguments = vec!["-E", "-P", "-x", self.language.flag()];
        let standard;
        if let Some(written) = self.standard.as_deref() {
            standard = format!("-std={written}");
            arguments.push(&standard);
        }
        let path = file.to_string_lossy().to_string();
        arguments.push(&path);

        let output = self.runner.run(&self.program, &arguments, &[]);
        let _ = std::fs::remove_file(&file);

        let output: Output = output?;
        if !output.succeeded {
            return None;
        }
        if output.stdout.contains("CPPLS_PROBE_YES") {
            Some(1)
        } else if output.stdout.contains("CPPLS_PROBE_NO") {
            Some(0)
        } else {
            None
        }
    }
}

impl FeatureProbe for CompilerFeatureProbe {
    fn answer(&self, operator: &str, operand: &str) -> Option<i128> {
        if !Self::is_a_question_this_probe_answers(operator)
            || !Self::is_a_plausible_operand(operand)
        {
            return None;
        }
        let standard = self.standard.as_deref();
        if let Some(answered) = self.cache.get(operator, operand, self.language, standard) {
            return answered;
        }
        let answered = self.ask(operator, operand);
        self.cache
            .put(operator, operand, self.language, standard, answered);
        answered
    }
}

/// **Where the probe file is written**, for a caller that wants to keep that directory off a shared disk — tests and
/// the analysis use the process's own temporary directory, which is what [`CompilerFeatureProbe`] does.
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A compiler that answers `YES` for one operand and `NO` for another, without being a compiler.
    struct Answers {
        yes: &'static str,
        runs: std::sync::atomic::AtomicUsize,
    }

    impl CommandRunner for Answers {
        fn run(
            &self,
            _program: &Path,
            arguments: &[&str],
            _environment: &[(std::ffi::OsString, std::ffi::OsString)],
        ) -> Option<Output> {
            self.runs.fetch_add(1, Ordering::Relaxed);
            let file = Path::new(arguments.last()?);
            let source = std::fs::read_to_string(file).ok()?;
            assert!(
                source.contains("__has_builtin"),
                "the question is in the file"
            );
            let stdout = if source.contains(self.yes) {
                "CPPLS_PROBE_YES"
            } else {
                "CPPLS_PROBE_NO"
            };
            Some(Output {
                stdout: stdout.to_string(),
                stderr: String::new(),
                status: Some(0),
                succeeded: true,
            })
        }
    }

    #[test]
    fn the_operator_is_asked_of_the_compiler_and_remembered() {
        let answers = Arc::new(Answers {
            yes: "__builtin_expect",
            runs: Default::default(),
        });
        let probe = CompilerFeatureProbe::new(
            PathBuf::from("cc"),
            answers.clone(),
            ProbeLanguage::C,
            Some("gnu11".to_string()),
        );

        assert_eq!(probe.answer("__has_builtin", "__builtin_expect"), Some(1));
        assert_eq!(
            probe.answer("__has_builtin", "__builtin_unreachable"),
            Some(0)
        );
        // A repeated question is a map lookup: the compiler is not run again.
        assert_eq!(probe.answer("__has_builtin", "__builtin_expect"), Some(1));
        assert_eq!(answers.runs.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn an_operand_that_is_not_a_name_is_not_pasted_into_a_source_file() {
        let answers = Arc::new(Answers {
            yes: "__builtin_expect",
            runs: Default::default(),
        });
        let probe = CompilerFeatureProbe::new(
            PathBuf::from("cc"),
            answers.clone(),
            ProbeLanguage::Cpp,
            None,
        );

        assert_eq!(
            probe.answer("__has_builtin", "__builtin_expect)\n#error"),
            None
        );
        assert_eq!(answers.runs.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn an_operator_this_probe_does_not_answer_is_left_to_the_caller() {
        let answers = Arc::new(Answers {
            yes: "x",
            runs: Default::default(),
        });
        let probe = CompilerFeatureProbe::new(PathBuf::from("cc"), answers, ProbeLanguage::C, None);

        assert_eq!(probe.answer("__has_include", "vector"), None);
    }
}
