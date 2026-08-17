//! Turning an `amk-types` send request into signed RFC 5322 bytes.
//!
//! `[SPEC:.claude/contracts/amk-outbound.md]`. `mail-builder` assembles; nothing it produces or
//! consumes crosses this module's public edge.

use amk_types::message::{Addresses, SendMessageRequest};

use crate::assemble::check_headers;
use crate::signing::Keyring;
use crate::{OutboundError, SignedMessage};

/// What a send needs beyond the caller's request: who is sending, and the threading position.
///
/// Separate from [`SendMessageRequest`] because the caller supplies the request and the *server*
/// supplies these — an inbox cannot choose to send as another inbox by putting a different `From`
/// in the body, and [`check_headers`] refuses a caller `From` for the same reason.
#[derive(Debug, Clone, Default)]
pub struct SendContext {
    /// The sending inbox. `inbox_id` IS the address (fixture 03).
    pub from: String,
    /// Set on a reply: the parent's Message-ID, already bracketed.
    pub in_reply_to: Option<String>,
    /// Set on a reply: the parent's `References` chain plus the parent itself.
    pub references: Vec<String>,
}

/// Assemble, sign, and return the bytes plus the envelope.
///
/// Order matters and is not arbitrary: headers are checked **before** anything is built, the
/// `Message-ID` is generated once and is the id returned, and the signature is computed over the
/// assembled bytes and prepended. Signing before assembly would sign something the recipient never
/// sees; generating the id twice would return an id that is not in the message.
pub fn build_signed(
    req: &SendMessageRequest,
    ctx: &SendContext,
    keys: &Keyring,
    message_id: &str,
) -> Result<SignedMessage, OutboundError> {
    if let Some(headers) = &req.headers {
        check_headers(headers)?;
    }

    let domain = ctx
        .from
        .rsplit_once('@')
        .map(|(_, d)| d.to_ascii_lowercase())
        .ok_or_else(|| OutboundError::Assembly(format!("sender {:?} has no domain", ctx.from)))?;

    let to = flatten(req.to.as_ref());
    let cc = flatten(req.cc.as_ref());
    let bcc = flatten(req.bcc.as_ref());
    if to.is_empty() && cc.is_empty() && bcc.is_empty() {
        // `[SPEC:reference/fixtures/05-error-catalog.http]` carries this as a whole-body rule:
        // "to, cc, or bcc must be specified". Enforced here so no message is ever assembled with
        // nobody to deliver it to.
        return Err(OutboundError::Assembly("to, cc, or bcc must be specified".to_owned()));
    }

    let mut builder = mail_builder::MessageBuilder::new()
        .from(ctx.from.as_str())
        .message_id(message_id.trim_matches(['<', '>']).to_owned());

    if !to.is_empty() {
        builder = builder.to(to.clone());
    }
    if !cc.is_empty() {
        builder = builder.cc(cc.clone());
    }
    // `bcc` is deliberately NOT handed to the builder. `mail_builder`'s `.bcc()` writes a `Bcc:`
    // header into the message, which discloses every blind recipient to every other recipient —
    // the exact opposite of what the field means. Verified, not assumed: a test asserted the
    // header was absent and failed against `.bcc()`. Blind recipients live in the ENVELOPE only,
    // which is what `envelope_to` below carries.
    if let Some(reply_to) = flatten_opt(req.reply_to.as_ref()) {
        builder = builder.reply_to(reply_to);
    }
    if let Some(subject) = &req.subject {
        builder = builder.subject(subject.as_str());
    }
    // Threading: `[SPEC:reference/fixtures/21-unbracketed-in-reply-to.txt]` and register C3 — the
    // reference re-brackets a parsed linkage value before matching, and `amk-core::threading`
    // implements that. What this must produce is a header that path can match, so the bracketed
    // form goes on the wire and the rule itself is not re-derived here.
    if let Some(parent) = &ctx.in_reply_to {
        builder = builder.in_reply_to(parent.trim_matches(['<', '>']).to_owned());
    }
    if !ctx.references.is_empty() {
        let refs: Vec<String> = ctx
            .references
            .iter()
            .map(|r| r.trim_matches(['<', '>']).to_owned())
            .collect();
        builder = builder.references(refs);
    }
    if let Some(text) = &req.text {
        builder = builder.text_body(text.as_str());
    }
    if let Some(html) = &req.html {
        builder = builder.html_body(html.as_str());
    }
    if let Some(headers) = &req.headers {
        for (name, value) in headers {
            builder =
                builder.header(name.as_str(), mail_builder::headers::raw::Raw::new(value.as_str()));
        }
    }

    let raw = builder
        .write_to_vec()
        .map_err(|e| OutboundError::Assembly(e.to_string()))?;

    let signature = keys.sign(&domain, &raw)?;
    let mut signed = Vec::with_capacity(signature.len() + raw.len());
    signed.extend_from_slice(signature.as_bytes());
    signed.extend_from_slice(&raw);

    // The envelope recipients are every addressee including `bcc` — that is what `bcc` MEANS: it
    // is removed from the headers a recipient sees, not from delivery.
    let mut envelope_to = to;
    envelope_to.extend(cc);
    envelope_to.extend(bcc);
    envelope_to.sort();
    envelope_to.dedup();

    Ok(SignedMessage {
        message_id: message_id.to_owned(),
        envelope_from: ctx.from.clone(),
        envelope_to,
        raw: signed,
    })
}

fn flatten(a: Option<&Addresses>) -> Vec<String> {
    a.cloned().map(Addresses::into_vec).unwrap_or_default()
}

fn flatten_opt(a: Option<&Addresses>) -> Option<Vec<String>> {
    let v = flatten(a);
    (!v.is_empty()).then_some(v)
}

/// Recipients for a reply-all: everyone on the parent except the sender itself.
///
/// **`[INFERRED]`** — no fixture captures the reference's reply-all derivation. What is implemented
/// is the conventional rule: the parent's `from` plus its `to` and `cc`, minus the sending inbox,
/// de-duplicated, case-folded for the comparison because an address that differs only in case is
/// the same mailbox. If a capture ever contradicts this, it is one function to change.
pub fn reply_all_recipients(
    parent_from: &str,
    parent_to: &[String],
    parent_cc: &[String],
    sending_inbox: &str,
) -> Vec<String> {
    let me = sending_inbox.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for addr in std::iter::once(parent_from)
        .chain(parent_to.iter().map(String::as_str))
        .chain(parent_cc.iter().map(String::as_str))
    {
        let folded = addr.to_ascii_lowercase();
        if folded == me || seen.contains(&folded) {
            continue;
        }
        seen.push(folded);
        out.push(addr.to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn keyring() -> Keyring {
        use base64::Engine as _;
        let wrapped: String = include_str!("testdata/test-signing-key.pkcs8.b64")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(wrapped)
            .unwrap();
        let mut k = Keyring::new();
        k.insert_der("example.test", "amk", &der).unwrap();
        k
    }

    fn req() -> SendMessageRequest {
        SendMessageRequest {
            to: Some(Addresses::One("you@other.test".into())),
            cc: None,
            bcc: None,
            reply_to: None,
            subject: Some("hi".into()),
            text: Some("body".into()),
            html: None,
            labels: vec![],
            attachments: vec![],
            headers: None,
        }
    }

    fn ctx() -> SendContext {
        SendContext { from: "me@example.test".into(), ..Default::default() }
    }

    fn as_text(m: &SignedMessage) -> String {
        String::from_utf8_lossy(&m.raw).into_owned()
    }

    #[test]
    fn a_send_is_signed_and_the_signature_precedes_the_message() {
        let m = build_signed(&req(), &ctx(), &keyring(), "<abc@example.test>").unwrap();
        let text = as_text(&m);
        assert!(text.starts_with("DKIM-Signature:"), "signature first: {}", &text[..80]);
        // `mail-builder` may render an address with or without angle brackets; assert the
        // mailbox is on a `From:` line rather than pinning a formatting choice it owns.
        assert!(
            text.lines()
                .any(|l| l.starts_with("From:") && l.contains("me@example.test")),
            "{text}"
        );
        assert!(text.contains("Subject: hi"), "{text}");
        assert_eq!(m.message_id, "<abc@example.test>");
        assert!(text.contains("abc@example.test"), "the returned id is IN the message: {text}");
    }

    #[test]
    fn a_domain_with_no_key_produces_no_message_at_all() {
        let c = SendContext { from: "me@unkeyed.test".into(), ..Default::default() };
        let err = build_signed(&req(), &c, &keyring(), "<a@unkeyed.test>").unwrap_err();
        assert!(
            matches!(err, OutboundError::NoSigningKey(ref d) if d == "unkeyed.test"),
            "{err}"
        );
    }

    /// `bcc` is removed from the headers a recipient sees and NOT from delivery — the whole point
    /// of the field. Both halves asserted, because an implementation that drops it from either one
    /// is wrong in a way the other half would not catch.
    #[test]
    fn bcc_reaches_the_envelope_and_not_the_headers() {
        let mut r = req();
        r.bcc = Some(Addresses::One("hidden@other.test".into()));
        let m = build_signed(&r, &ctx(), &keyring(), "<b@example.test>").unwrap();
        assert!(
            m.envelope_to.contains(&"hidden@other.test".to_string()),
            "bcc must be delivered: {:?}",
            m.envelope_to
        );
        assert!(!as_text(&m).contains("hidden@other.test"), "bcc must not be in the headers");
    }

    #[test]
    fn a_send_with_no_recipient_at_all_is_refused_before_assembly() {
        let mut r = req();
        r.to = None;
        let err = build_signed(&r, &ctx(), &keyring(), "<c@example.test>").unwrap_err();
        assert!(matches!(err, OutboundError::Assembly(ref m) if m.contains("must be specified")));
    }

    /// The injection vector, end to end rather than at the sanitiser alone: a caller header
    /// carrying CRLF must not become two headers in the assembled bytes.
    #[test]
    fn a_crlf_bearing_caller_header_never_reaches_the_assembled_bytes() {
        let mut r = req();
        let mut h = BTreeMap::new();
        h.insert("X-Evil".to_owned(), "a\r\nBcc: attacker@evil.test".to_owned());
        r.headers = Some(h);
        let err = build_signed(&r, &ctx(), &keyring(), "<d@example.test>").unwrap_err();
        assert!(matches!(err, OutboundError::ForbiddenHeader(_)), "{err}");
    }

    #[test]
    fn a_caller_supplied_from_cannot_override_the_sending_inbox() {
        let mut r = req();
        let mut h = BTreeMap::new();
        h.insert("From".to_owned(), "someone@else.test".to_owned());
        r.headers = Some(h);
        assert!(build_signed(&r, &ctx(), &keyring(), "<e@example.test>").is_err());
    }

    /// Register C3 / fixture 21: the bracketed form goes on the wire so `amk-core::threading` can
    /// match it. Asserted on the assembled bytes, not on the context struct.
    #[test]
    fn a_reply_carries_bracketed_linkage_headers() {
        let c = SendContext {
            from: "me@example.test".into(),
            in_reply_to: Some("<parent@other.test>".into()),
            references: vec!["<root@other.test>".into(), "<parent@other.test>".into()],
        };
        let text = as_text(&build_signed(&req(), &c, &keyring(), "<r@example.test>").unwrap());
        assert!(text.contains("In-Reply-To: <parent@other.test>"), "{text}");
        assert!(text.contains("root@other.test"), "{text}");
        assert!(text.contains("References:"), "{text}");
    }

    /// An unbracketed parent id still produces a bracketed header — the same coercion register C3
    /// applied to threading, on the send side.
    #[test]
    fn an_unbracketed_parent_id_is_bracketed_on_the_wire() {
        let c = SendContext {
            from: "me@example.test".into(),
            in_reply_to: Some("parent@other.test".into()),
            references: vec![],
        };
        let text = as_text(&build_signed(&req(), &c, &keyring(), "<r2@example.test>").unwrap());
        assert!(text.contains("In-Reply-To: <parent@other.test>"), "{text}");
    }

    #[test]
    fn reply_all_excludes_the_sender_dedupes_and_folds_case() {
        let out = reply_all_recipients(
            "alice@other.test",
            &["ME@example.test".into(), "bob@other.test".into()],
            &["Bob@other.test".into(), "carol@other.test".into()],
            "me@example.test",
        );
        assert_eq!(out, vec!["alice@other.test", "bob@other.test", "carol@other.test"]);
    }

    #[test]
    fn reply_all_on_a_message_only_to_us_still_replies_to_the_sender() {
        let out = reply_all_recipients(
            "alice@other.test",
            &["me@example.test".into()],
            &[],
            "me@example.test",
        );
        assert_eq!(out, vec!["alice@other.test"]);
    }
}
