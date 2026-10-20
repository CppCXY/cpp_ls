//! # `textDocument/signatureHelp` — which function is being called, and which parameter is being typed
//!
//! ```text
//! make(|)              →  make(int count, double factor)     the first parameter is active
//! make(1, |)           →  make(int count, double factor)     the second
//! std::format(|)       →  four signatures, and the reader cycles
//! ```
//!
//! The analysis answers with the declarations' own text ([`Session::signatures_at`]) and this layer renders them:
//! each signature's parameters with their spans inside the label, and the index of the one the cursor is in.
//!
//! # Why a list, and what it does not claim
//!
//! The protocol allows a list of overloads with an `activeSignature`, and this server used to send **one** — on the
//! argument that choosing between candidates for a half-typed argument list *is* overload resolution. That argument
//! is sound about *choosing* and it was the wrong conclusion: the alternative to a list is not one signature, it is
//! none. `std::format` is four declarations, the name therefore answered `Ambiguous`, and a reader typing
//! `std::format(` got an empty popup on the most ordinary call in modern C++. Sending all four claims nothing —
//! the client shows them stacked and the reader picks, which is what `activeSignature` is for.
//!
//! # Where the parameters come from
//!
//! The callee's declarations, so a call into a header parses that header for this answer — once per file, however
//! many overloads it holds. The types are the written spellings (`const Widget&` is those three tokens, an alias is
//! its own name), and the documentation above each declaration travels with its signature — that is the popup's
//! whole value: the parameter being typed is the one the `@param` line describes.
//!
//! Nothing is answered while the index is still being read: which call this is depends on where the callee is
//! declared, and a signature for the wrong declaration would be confidently wrong rather than missing.

use lsp_types::{
    ClientCapabilities, Documentation, MarkupContent, MarkupKind, ParameterInformation,
    ParameterLabel, ServerCapabilities, SignatureHelp, SignatureHelpOptions, SignatureHelpParams,
    SignatureInformation,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use cpp_parser::CppDocComment;
use crate::util::{offset_at_position, uri_to_file_path};

pub async fn on_signature_help(
    context: ServerContextSnapshot,
    params: SignatureHelpParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<SignatureHelp> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;

        // `std::format(` in a file that says `import std;` needs the same read the hover and the definition do — and
        // this handler has its own reason to care: it declines to answer while files are queued, and a module whose
        // interface unit nobody read is exactly that state. `false`: no edit to catch up on.
        crate::handlers::read_the_modules(&context, &path, false).await;
        // **And then the query, which no longer waits for the project's index to drain.**
        //
        // The wait was `AnalysisState::settle`, asking whether the *whole project's* indexing queue was empty —
        // because the callee's declaration is usually in a header whose facts arrive later. What the answer needs is
        // this file's own closure, and `read_the_modules` above is what reads it (it catches the summary up first).
        // See `docs/status.md` §5.2.
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let offset = offset_at_position(&view, position)?;

        if !session.is_locally_ready(&path) {
            log::debug!(
                "no signature yet: {} file(s) are queued, and where the callee is declared is not known",
                session.pending()
            );
            return None;
        }

        let signatures = session.signatures_at(&view, offset);
        if signatures.is_empty() {
            return None;
        }

        Some(signature_help_of(&signatures))
    })
    .await
}

/// The signatures of one call, as the protocol's shape.
///
/// A function of its own rather than a closure in the handler, because the four decisions worth testing directly
/// are here: which span each parameter occupies **inside the label** (a client bolds it), which one is active, which
/// signature is active, and that the documentation is rendered the same way the hover renders it.
pub fn signature_help_of(
    signatures: &[cpp_code_analysis::signature::CallSignature],
) -> SignatureHelp {
    let rendered: Vec<SignatureInformation> = signatures
        .iter()
        .map(|signature| SignatureInformation {
            label: signature.label.clone(),
            // **The function's own text, without its parameter entries.**
            //
            // The protocol draws this above the signature, and `@param value the value` is a sentence about *one*
            // argument — it belongs under that argument, which is where `parameters[..].documentation` goes below.
            // The whole comment shown twice said the same thing twice, and the second time in the wrong place.
            documentation: signature
                .documentation
                .as_ref()
                .and_then(brief_of)
                .map(|value| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value,
                    })
                }),
            parameters: Some(
                signature
                    .parameters
                    .iter()
                    .map(|(range, written)| ParameterInformation {
                        // Offsets inside the label rather than a substring: the protocol's two shapes are
                        // equivalent, and offsets keep the parameter's *own* text in the label where the reader sees
                        // the declaration's spelling.
                        label: ParameterLabel::LabelOffsets([range.start as u32, range.end as u32]),
                        // **The `@param` line for this parameter**, when the declaration has one — the reason a
                        // reader opens this popup at all: the parameter being typed is the one documented.
                        documentation: signature
                            .documentation
                            .as_ref()
                            .and_then(|comment| parameter_documentation(comment, written))
                            .map(|value| {
                                Documentation::MarkupContent(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value,
                                })
                            }),
                    })
                    .collect(),
            ),
            // **Per signature**, which is the field that survives when a client lets the reader cycle: the second
            // overload of `format` does not take the same number of parameters as the first, and a single count for
            // the popup would highlight the wrong one the moment the reader switched.
            active_parameter: signature.active_parameter.map(|at| at as u32),
        })
        .collect();

    SignatureHelp {
        // The first is the active one, and it is stated rather than left out: a client that has to choose would
        // have nothing to choose between. Which of them the *language* would choose is overload resolution, and
        // this layer does not do it — see the module documentation.
        active_parameter: rendered.first().and_then(|first| first.active_parameter),
        signatures: rendered,
        active_signature: Some(0),
    }
}

/// **The documentation above a declaration, minus the sentences about its parameters.**
///
/// `@param value the value` documents one argument, and it is shown under that argument (see
/// [`parameter_documentation`]). Kept in the comment as well, the same sentence appeared twice — once in the block a
/// reader skims and once where they need it — and the block is the wrong place for it: what belongs above a
/// signature is what the *function* does.
///
/// The filter is by command kind rather than by spelling, so `@param[in] x` and `\\param x` go too. The rest of the
/// comment is kept exactly as written, including the commands that are prose (`@note`, `@warning`): a reader of
/// `@note` wants it in the block.
fn brief_of(comment: &CppDocComment) -> Option<String> {
    if !comment.is_documentation() {
        return None;
    }

    use cpp_parser::DocCommandKind;

    // The text with those lines taken out. Line-shaped rather than character-shaped because a doc comment *is*
    // line-shaped: every command starts a line's content, and removing one means removing that line.
    let text = comment.get_comment_text();
    let mut kept = String::with_capacity(text.len());
    for line in text.lines() {
        let content = line.trim_start().trim_start_matches("///").trim_start();
        let content = content.strip_prefix("*").map_or(content, |rest| rest.trim_start());
        let name = content.strip_prefix(['@', '\\']).map(|rest| {
            rest.chars()
                .take_while(|character| character.is_ascii_alphanumeric())
                .collect::<String>()
        });
        let is_a_parameter_line = name.as_deref().and_then(cpp_parser::command_kind)
            == Some(DocCommandKind::Parameter);

        if !is_a_parameter_line {
            kept.push_str(line);
            kept.push('\n');
        }
    }

    let kept = kept.trim();
    (!kept.is_empty()).then(|| kept.to_string())
}

/// **What one parameter is documented to be** — the body of the `@param`/`@tparam` line that names it.
///
/// The name is matched against the argument the grammar already split out of the command rather than against the
/// text, so `@param x  desc` (two spaces) and a description continued on the next line both work. `written` is the
/// parameter as the declaration spells it — `const Widget& widget` — and the *name* is its last word, which is what
/// a `@param` names.
fn parameter_documentation(comment: &CppDocComment, written: &str) -> Option<String> {
    use cpp_parser::DocCommandKind;

    let name = written.split_whitespace().last()?;
    let name = name.trim_matches(['*', '&']);

    comment
        .get_commands_of_kind(DocCommandKind::Parameter)
        .find(|command| command.get_argument_text().as_deref() == Some(name))
        .and_then(|command| command.get_body_text())
        .map(|body| body.trim().to_string())
        .filter(|body| !body.is_empty())
}

pub struct SignatureHelpCapabilities;
impl RegisterCapabilities for SignatureHelpCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.signature_help_provider = Some(SignatureHelpOptions {
            // `(` opens a call and `,` moves to the next parameter — the two moments a reader wants this.
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: Some(vec![",".to_string()]),
            work_done_progress_options: Default::default(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_code_analysis::signature::CallSignature;
    use std::path::PathBuf;

    fn a_signature() -> CallSignature {
        // The spans are *derived from the label* rather than counted by hand: an offset that is one out is a test
        // that fails for a reason that has nothing to do with the handler.
        let label = "scale(int count, double factor)".to_string();
        let span = |text: &str| {
            let start = label.find(text).expect("the fixture writes it");
            start..start + text.len()
        };

        CallSignature {
            parameters: vec![
                (span("int count"), "int count".to_string()),
                (span("double factor"), "double factor".to_string()),
            ],
            label,
            active_parameter: Some(1),
            declared_in: PathBuf::from("/p/a.cpp"),
            declared_at: 7,
            documentation: None,
        }
    }

    /// **The documentation is split in two**: the function's own text above the signature, and each `@param` under
    /// the parameter it names.
    ///
    /// Reported from an editor: "the function's own popup should be synthesized from the doc and not include the
    /// `@param` lines, and the parameters themselves should get their own". Before this the whole comment was shown
    /// above the signature and every parameter had `documentation: None` — the same sentence twice, the second time
    /// nowhere.
    #[test]
    fn a_doc_comment_is_split_between_the_signature_and_its_parameters() {
        use cpp_parser::CppAstNode as _;

        let text = "\
/// Scales a value by a factor, keeping the sign.
///
/// @param count how many times to scale
/// @param factor what to multiply by
/// @return the scaled value
double scale(int count, double factor);
";
        let tree = cpp_parser::CppParser::parse(text, cpp_parser::ParserConfig::default());
        let root = tree.get_red_root();
        let comment = root
            .descendants()
            .find(|node| {
                cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::DocComment
            })
            .and_then(CppDocComment::cast)
            .expect("the fixture writes a doc comment");

        let brief = brief_of(&comment).expect("there is prose");
        println!("brief: {brief:?}");
        assert!(
            brief.contains("Scales a value by a factor"),
            "the function's own sentence is kept: {brief:?}"
        );
        assert!(
            !brief.contains("@param"),
            "and the parameter lines are not repeated above the signature: {brief:?}"
        );
        assert!(
            brief.contains("@return"),
            "a command that is prose stays: {brief:?}"
        );

        assert_eq!(
            parameter_documentation(&comment, "int count").as_deref(),
            Some("how many times to scale")
        );
        assert_eq!(
            parameter_documentation(&comment, "double factor").as_deref(),
            Some("what to multiply by")
        );
        assert_eq!(
            parameter_documentation(&comment, "int missing"),
            None,
            "a parameter with no `@param` line has no documentation rather than somebody else's"
        );
    }

    /// **The parameter spans are offsets into the label**, and they say what the declaration said — checked by
    /// slicing the label with them, which is the only thing a client does with them.
    #[test]
    fn a_parameter_is_a_span_inside_the_label() {
        let help = signature_help_of(&[a_signature()]);

        assert_eq!(help.signatures.len(), 1);
        let label = &help.signatures[0].label;
        let spans: Vec<&str> = help.signatures[0]
            .parameters
            .as_ref()
            .expect("the parameters are sent")
            .iter()
            .map(|parameter| match &parameter.label {
                ParameterLabel::LabelOffsets([start, end]) => {
                    &label[*start as usize..*end as usize]
                }
                other => panic!("offsets were expected, not {other:?}"),
            })
            .collect();

        assert_eq!(spans, vec!["int count", "double factor"]);
        assert_eq!(help.active_signature, Some(0));
        assert_eq!(help.active_parameter, Some(1));
    }

    /// A signature with no documentation sends none, rather than an empty popup section.
    #[test]
    fn a_signature_without_documentation_sends_none() {
        let help = signature_help_of(&[a_signature()]);
        assert!(help.signatures[0].documentation.is_none());
    }

    /// **An overload set is sent as a list**, each signature with its own active parameter — so a client that lets
    /// the reader cycle between them highlights the right one after the switch, and the popup is not empty.
    #[test]
    fn every_overload_is_sent_with_its_own_active_parameter() {
        let first = a_signature();
        let second = CallSignature {
            label: "scale(double factor)".to_string(),
            parameters: vec![(9..9 + "double factor".len(), "double factor".to_string())],
            active_parameter: None,
            ..first.clone()
        };

        let help = signature_help_of(&[first, second]);

        assert_eq!(help.signatures.len(), 2);
        assert_eq!(help.signatures[0].label, "scale(int count, double factor)");
        assert_eq!(help.signatures[1].label, "scale(double factor)");
        assert_eq!(
            help.signatures[0].active_parameter,
            Some(1),
            "the signature's own count, not the popup's"
        );
        assert_eq!(help.signatures[1].active_parameter, None);
        assert_eq!(
            help.active_parameter,
            Some(1),
            "and the first is the active one"
        );
    }
}
