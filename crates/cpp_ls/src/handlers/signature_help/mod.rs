//! # `textDocument/signatureHelp` — which function is being called, and which parameter is being typed
//!
//! ```text
//! make(|)              →  make(int count, double factor)     the first parameter is active
//! make(1, |)           →  make(int count, double factor)     the second
//! ```
//!
//! The analysis answers with the declaration's own text ([`Session::signature_at`]) and this layer renders it:
//! one signature, each parameter's span inside the label, and the index of the one the cursor is in.
//!
//! # One signature, and why that is the honest answer
//!
//! The protocol allows a list of overloads with an `activeSignature`. This server sends **one**, because one is
//! what it resolved: the same lookup the definition, the hover and the parameter hints use. With several
//! declarations of a name the analysis either resolves to one of them or declines with `Ambiguous`, and a declined
//! call answers nothing here rather than a list of every candidate — choosing between them for a half-typed
//! argument list *is* overload resolution, and a signature list the reader picks from must not be a guess.
//!
//! # Where the parameters come from
//!
//! The callee's declaration, so a call into a header parses that header for this answer. The types are the written
//! spellings (`const Widget&` is those three tokens, an alias is its own name), and the documentation above the
//! declaration travels with the signature — that is the popup's whole value: the parameter being typed is the one
//! the `@param` line describes.
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
use crate::handlers::hover::documentation_text;
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
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let offset = offset_at_position(&view, position)?;

        if session.pending() > 0 {
            log::debug!(
                "no signature yet: {} file(s) are queued, and where the callee is declared is not known",
                session.pending()
            );
            return None;
        }

        let signature = session.signature_at(&view, offset)?;
        Some(signature_help_of(&signature))
    })
    .await
}

/// One signature, as the protocol's shape.
///
/// A function of its own rather than a closure in the handler, because the three decisions worth testing directly
/// are here: which span each parameter occupies **inside the label** (a client bolds it), which one is active, and
/// that the documentation is rendered the same way the hover renders it.
pub fn signature_help_of(signature: &cpp_code_analysis::signature::CallSignature) -> SignatureHelp {
    let parameters: Vec<ParameterInformation> = signature
        .parameters
        .iter()
        .map(|(range, _)| ParameterInformation {
            // Offsets inside the label rather than a substring: the protocol's two shapes are equivalent, and
            // offsets keep the parameter's *own* text in the label where the reader sees the declaration's
            // spelling.
            label: ParameterLabel::LabelOffsets([range.start as u32, range.end as u32]),
            documentation: None,
        })
        .collect();

    SignatureHelp {
        signatures: vec![SignatureInformation {
            label: signature.label.clone(),
            documentation: signature
                .documentation
                .as_ref()
                .and_then(documentation_text)
                .map(|value| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value,
                    })
                }),
            parameters: Some(parameters),
            active_parameter: None,
        }],
        // One signature, so the active one is the only one — and it is stated rather than left out, because a
        // client that has to choose would have nothing to choose between.
        active_signature: Some(0),
        active_parameter: signature.active_parameter.map(|at| at as u32),
    }
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

    /// **The parameter spans are offsets into the label**, and they say what the declaration said — checked by
    /// slicing the label with them, which is the only thing a client does with them.
    #[test]
    fn a_parameter_is_a_span_inside_the_label() {
        let help = signature_help_of(&a_signature());

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
        let help = signature_help_of(&a_signature());
        assert!(help.signatures[0].documentation.is_none());
    }
}
