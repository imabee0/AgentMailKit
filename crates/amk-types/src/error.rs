//! The two error shapes, and the code catalog.
//!
//! P-1 item 5 (`reference/fixtures/05-error-catalog.http`) established that AgentMail returns
//! **two structurally different** error bodies, and a clone must reproduce both:
//!
//! 1. **Auth layer** (missing or unusable credential) → a bare gateway body with only `message`:
//!    `401 {"message":"Unauthorized"}` / `403 {"message":"Forbidden"}`. This holds even for a
//!    *well-formed but unknown* `am_` key, so it is not merely a malformed-token path.
//! 2. **Application layer** → the full envelope `{name, code, message, fix?, docs?}`, plus
//!    per-code extras (`errors[]` on `validation_error`, `suggestions[]` on `already_exists`).
//!
//! Clients are told to branch on `code`; `name`/`message` retain legacy pre-`code` values.

use serde::{Deserialize, Serialize};

/// Bare body returned by the auth layer. Carries `message` and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayError {
    pub message: String,
}

impl GatewayError {
    pub fn unauthorized() -> Self {
        Self { message: "Unauthorized".into() }
    }
    pub fn forbidden() -> Self {
        Self { message: "Forbidden".into() }
    }
}

/// One entry of `validation_error`'s `errors` array.
///
/// `{code, path[], message}` is the FLOOR, not the shape.
/// `[SPEC:reference/fixtures/27-malformed-request-handling.txt]` probed five malformed requests
/// against the live API and every entry carried extras keyed to its own `code` — the same
/// discovery [`ErrorEnvelope`] already made with `suggestions[]` and `resource`/`limit`
/// (register B5), one level further down:
///
/// | `code`           | extras observed                     |
/// |------------------|-------------------------------------|
/// | `custom`         | none (fixture 05)                   |
/// | `invalid_type`   | `expected`, `received` (sometimes)  |
/// | `too_small`      | `origin`, `minimum`, `inclusive`    |
/// | `invalid_value`  | `expected`, `values[]`              |
/// | `invalid_format` | `format`                            |
///
/// Modelled as explicit optional fields rather than a catch-all map, matching how [`ErrorEnvelope`]
/// models its own per-code extras: a named field is checkable and a map is not. These are the kinds
/// this server EMITS; the vocabulary is zod's and is larger, so a kind we have never produced has
/// no field here and adding one is a fixture-backed change, not a completion of the pattern.
///
/// `received` is `Option` for a reason that is easy to get backwards: the live capture carries it
/// for `?limit=abc` (`"received":"NaN"`) and OMITS it for `{"name":123}`. Both are `invalid_type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub code: String,
    /// `["<field>"]` for a field-level failure; `[]` **only** for a whole-body rule. Fixture 05's
    /// empty path is the whole-body case ("to, cc, or bcc must be specified"), not the general one
    /// — reading it as general is what produced a first draft that never named a field.
    pub path: Vec<serde_json::Value>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inclusive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

impl ValidationIssue {
    /// The bare kind, for a rule that spans the whole body and names no field.
    /// `[SPEC:reference/fixtures/05-error-catalog.http]`
    pub fn custom(message: impl Into<String>) -> Self {
        Self {
            code: "custom".into(),
            path: Vec::new(),
            message: message.into(),
            expected: None,
            received: None,
            origin: None,
            minimum: None,
            inclusive: None,
            format: None,
            values: Vec::new(),
        }
    }

    /// A value whose FORMAT is wrong: an unparseable body (`format: "json_string"`, empty path) or
    /// a malformed `page_token` (`format: "base64url"`, `path: ["page_token"]`).
    /// `[SPEC:reference/fixtures/27-malformed-request-handling.txt]`
    pub fn invalid_format(format: &str, field: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            code: "invalid_format".into(),
            format: Some(format.to_owned()),
            ..Self::at(field, message)
        }
    }

    /// A value of the wrong JSON type. `expected` is the type the schema wanted; `received` is
    /// what arrived, and is omitted when the reference omits it.
    /// `[SPEC:reference/fixtures/27-malformed-request-handling.txt]`
    pub fn invalid_type(
        expected: &str,
        received: Option<&str>,
        field: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: "invalid_type".into(),
            expected: Some(expected.to_owned()),
            received: received.map(str::to_owned),
            ..Self::at(field, message)
        }
    }

    /// A number below its bound. `inclusive` is `false` in every capture: `?limit=0` is rejected,
    /// so the bound is `> minimum`, not `>=`.
    /// `[SPEC:reference/fixtures/27-malformed-request-handling.txt]`
    pub fn too_small(
        origin: &str,
        minimum: i64,
        inclusive: bool,
        field: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: "too_small".into(),
            origin: Some(origin.to_owned()),
            minimum: Some(minimum),
            inclusive: Some(inclusive),
            ..Self::at(field, message)
        }
    }

    /// A value outside a closed set, which the reference ENUMERATES back to the caller.
    /// `[SPEC:reference/fixtures/27-malformed-request-handling.txt]`
    pub fn invalid_value(
        expected: &str,
        values: Vec<String>,
        field: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: "invalid_value".into(),
            expected: Some(expected.to_owned()),
            values,
            ..Self::at(field, message)
        }
    }

    /// Shared spine: code is overwritten by each constructor above, so this is private.
    fn at(field: Option<&str>, message: impl Into<String>) -> Self {
        let mut issue = Self::custom(message);
        if let Some(name) = field {
            issue.path = vec![serde_json::Value::String(name.to_owned())];
        }
        issue
    }
}

/// The application-layer error envelope.
///
/// Field order matches the live responses. Optional members are omitted entirely when absent —
/// never emitted as `null` or `""` (observed: `sub_type` omitted, not blanked).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    /// Legacy human name, e.g. `NotFoundError`, `AlreadyExistsError`, `ValidationError`.
    pub name: String,
    /// The stable machine key. **Branch on this**, not on `name`/`message`.
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ValidationIssue>,
    /// `already_exists` on inbox creation returns available alternatives here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// `limit_exceeded` extras, observed together in
    /// `reference/fixtures/18-inbox-case-normalization.txt`: `"resource":"inbox","limit":3`.
    ///
    /// A third per-code extra set, after `validation_error`'s `errors[]` and `already_exists`'
    /// `suggestions[]` — which is the point: the envelope's extras are **per code**, not a fixed
    /// set, so `type_:ErrorResponse` is a floor rather than a ceiling. Neither this pair nor
    /// `suggestions` appears in any openapi schema; both are live-only.
    ///
    /// The same body carries `upgrade_url`, which we deliberately do **not** model — no billing
    /// surface. That omission is a decision, recorded here so nobody later "completes" the shape.
    /// The quota itself stays real: a self-hosted deployment may still impose a configured cap,
    /// and it is counted organization-wide, not per pod.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
}

/// The envelope `message` every observed `validation_error` carries. The SPECIFIC text lives in
/// `errors[]` — one entry per violated rule — not here.
/// `[SPEC:fixture 05-error-catalog.http]`, n=1 but unambiguous:
/// `"message":"Request validation failed"` beside
/// `"errors":[{"code":"custom","path":[],"message":"to, cc, or bcc must be specified"}]`.
pub const VALIDATION_MESSAGE: &str = "Request validation failed";

impl ErrorEnvelope {
    /// `validation_error` is the one code whose envelope shape is not "message says it all": the
    /// live API fixes `message` to [`VALIDATION_MESSAGE`] and carries the specific rule in
    /// `errors[]`, whose `path` is `[]` for a whole-body rule. So a caller's specific text is
    /// routed there rather than into `message`, and `errors` is never empty for this code.
    ///
    /// Routed HERE rather than left to each call site, for the reason `ErrorCode::status()` is:
    /// four sites constructed this envelope and **none** of them populated `errors`, so every
    /// `validation_error` this server emitted violated both the observed shape and the spec's own
    /// `ValidationErrorResponse.required = [name, errors]` — while emitting a `fix` string that
    /// tells the client to "inspect the errors array" we did not send. Found by the P1 schemathesis
    /// run, not by any hand-written test, because every hand-written test asserted the `message`
    /// its own call site had just passed in.
    ///
    /// A caller with structured, per-field issues overrides the synthesized entry with
    /// [`ErrorEnvelope::with_issues`].
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let message = message.into();
        let (message, errors) = if matches!(code, ErrorCode::ValidationError) {
            (VALIDATION_MESSAGE.to_owned(), vec![ValidationIssue::custom(message)])
        } else {
            (message, Vec::new())
        };
        Self {
            name: code.legacy_name().to_owned(),
            code,
            message,
            errors,
            suggestions: Vec::new(),
            fix: None,
            resource: None,
            limit: None,
            docs: Some(code.docs_url()),
        }
    }
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
    /// `limit_exceeded`'s per-code extras. Paired in one builder because they were observed
    /// together and neither is meaningful alone.
    pub fn with_limit(mut self, resource: impl Into<String>, limit: u64) -> Self {
        self.resource = Some(resource.into());
        self.limit = Some(limit);
        self
    }
    pub fn with_suggestions(mut self, s: Vec<String>) -> Self {
        self.suggestions = s;
        self
    }
    pub fn with_issues(mut self, e: Vec<ValidationIssue>) -> Self {
        self.errors = e;
        self
    }
    /// HTTP status this code is served with.
    pub fn status(&self) -> u16 {
        self.code.status()
    }
}

/// The documented code catalog (docs.agentmail.to/errors), with statuses corrected where the
/// live API disagreed with the docs — see the note on [`ErrorCode::AlreadyExists`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // --- 401 (emitted as the bare GatewayError body, not this envelope) ---
    MissingAuthorization,
    InvalidTokenType,
    UnknownApiKey,
    Unauthorized,
    // --- 403 ---
    MissingPermission,
    PermissionEscalation,
    UnrestrictedKeyRequired,
    Forbidden,
    MessageRejected,
    /// **Observed at HTTP 403** for a duplicate inbox username, with `suggestions[]`.
    /// The docs' `resource_taken`/409 and the SDK-derived 422 were both wrong
    /// (`reference/fixtures/05-error-catalog.http`).
    AlreadyExists,
    ResourceTaken,
    LimitExceeded,
    DomainNotVerified,
    // --- 400 / 404 / 422 ---
    ValidationError,
    NotFound,
    Unprocessable,
    QueryRangeTooWide,
    // --- 409 ---
    Conflict,
    RaceCondition,
    ResourceDeleting,
    /// **Observed at HTTP 409**, not the 403 the docs imply
    /// (`reference/fixtures/22-org-mount-and-delete-semantics.txt`: deleting a pod that still owns
    /// an inbox). `cannot_delete` appears **zero times** in `reference/openapi.json`, so the
    /// original 403 came from the docs page rather than the spec, and the live capture beats both.
    /// It belongs at 409 on the merits too: 403 says "you may not", 409 says "the resource's
    /// current state forbids it", and this is the second. The refusal is total — neither the pod
    /// nor its inbox is touched.
    CannotDelete,
    // --- 429 / 5xx ---
    RateLimitExceeded,
    ServiceUnavailable,
    InternalError,
}

impl ErrorCode {
    pub fn status(self) -> u16 {
        use ErrorCode::*;
        match self {
            MissingAuthorization | InvalidTokenType | UnknownApiKey | Unauthorized => 401,
            MissingPermission
            | PermissionEscalation
            | UnrestrictedKeyRequired
            | Forbidden
            | MessageRejected
            | AlreadyExists
            | ResourceTaken
            | LimitExceeded
            | DomainNotVerified => 403,
            ValidationError | QueryRangeTooWide => 400,
            NotFound => 404,
            Unprocessable => 422,
            Conflict | RaceCondition | ResourceDeleting | CannotDelete => 409,
            RateLimitExceeded => 429,
            ServiceUnavailable => 503,
            InternalError => 500,
        }
    }

    /// The legacy `name` field paired with each code in live responses.
    pub fn legacy_name(self) -> &'static str {
        use ErrorCode::*;
        match self {
            NotFound => "NotFoundError",
            AlreadyExists => "AlreadyExistsError",
            ValidationError => "ValidationError",
            Conflict | RaceCondition | ResourceDeleting => "ConflictError",
            Unprocessable => "UnprocessableError",
            MessageRejected => "MessageRejectedError",
            RateLimitExceeded => "RateLimitError",
            // Observed live in fixture 18. It had been falling through to the wildcard below,
            // which is the hazard of a wildcard in a table derived from captures: a code with a
            // real observed name silently gets the generic one, and nothing fails.
            LimitExceeded => "LimitExceededError",
            InternalError | ServiceUnavailable => "InternalError",
            _ => "Error",
        }
    }

    pub fn as_str(self) -> &'static str {
        use ErrorCode::*;
        match self {
            MissingAuthorization => "missing_authorization",
            InvalidTokenType => "invalid_token_type",
            UnknownApiKey => "unknown_api_key",
            Unauthorized => "unauthorized",
            MissingPermission => "missing_permission",
            PermissionEscalation => "permission_escalation",
            UnrestrictedKeyRequired => "unrestricted_key_required",
            Forbidden => "forbidden",
            MessageRejected => "message_rejected",
            AlreadyExists => "already_exists",
            ResourceTaken => "resource_taken",
            LimitExceeded => "limit_exceeded",
            DomainNotVerified => "domain_not_verified",
            CannotDelete => "cannot_delete",
            ValidationError => "validation_error",
            NotFound => "not_found",
            Unprocessable => "unprocessable",
            QueryRangeTooWide => "query_range_too_wide",
            Conflict => "conflict",
            RaceCondition => "race_condition",
            ResourceDeleting => "resource_deleting",
            RateLimitExceeded => "rate_limit_exceeded",
            ServiceUnavailable => "service_unavailable",
            InternalError => "internal_error",
        }
    }

    pub fn docs_url(self) -> String {
        format!("https://docs.agentmail.to/errors#{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_body_is_message_only() {
        // Live: no auth -> 401 {"message":"Unauthorized"}; bad key -> 403 {"message":"Forbidden"}.
        assert_eq!(
            serde_json::to_string(&GatewayError::unauthorized()).unwrap(),
            r#"{"message":"Unauthorized"}"#
        );
        assert_eq!(
            serde_json::to_string(&GatewayError::forbidden()).unwrap(),
            r#"{"message":"Forbidden"}"#
        );
    }

    #[test]
    fn not_found_envelope_matches_live_capture() {
        let live = r#"{"name":"NotFoundError","code":"not_found","message":"Inbox not found",
            "fix":"No inbox with the given identifier is visible to this credential.",
            "docs":"https://docs.agentmail.to/errors#not_found"}"#;
        let parsed: ErrorEnvelope = serde_json::from_str(live).unwrap();
        assert_eq!(parsed.code, ErrorCode::NotFound);
        assert_eq!(parsed.status(), 404);
        assert!(parsed.errors.is_empty() && parsed.suggestions.is_empty());
    }

    #[test]
    fn already_exists_is_403_with_suggestions() {
        // Live capture: duplicate inbox username.
        let live = r#"{"name":"AlreadyExistsError","code":"already_exists",
            "message":"Inbox already exists","fix":"...",
            "suggestions":["amk-probe4991","amk-probe6813","amk-probe9732"],
            "docs":"https://docs.agentmail.to/errors#already_exists"}"#;
        let parsed: ErrorEnvelope = serde_json::from_str(live).unwrap();
        assert_eq!(parsed.code, ErrorCode::AlreadyExists);
        assert_eq!(parsed.status(), 403, "observed 403, not 409/422");
        assert_eq!(parsed.suggestions.len(), 3);
    }

    #[test]
    fn limit_exceeded_carries_resource_and_limit_and_names_itself() {
        // Verbatim from reference/fixtures/18-inbox-case-normalization.txt section 4, minus
        // upgrade_url — which the live body carries and we deliberately do not model.
        let live = r#"{"name":"LimitExceededError","code":"limit_exceeded",
            "message":"Inbox limit exceeded","fix":"Your plan's inbox limit is 3. ...",
            "resource":"inbox","limit":3,"upgrade_url":"<url>",
            "docs":"https://docs.agentmail.to/errors#limit_exceeded"}"#;
        let parsed: ErrorEnvelope = serde_json::from_str(live).unwrap();
        assert_eq!(parsed.code, ErrorCode::LimitExceeded);
        assert_eq!(parsed.status(), 403);
        assert_eq!(parsed.resource.as_deref(), Some("inbox"));
        assert_eq!(parsed.limit, Some(3));
        // The name is the half that regressed: LimitExceeded fell through legacy_name's wildcard
        // and emitted the generic "Error" while every other observed code got its real name.
        assert_eq!(ErrorCode::LimitExceeded.legacy_name(), "LimitExceededError");
        assert_eq!(parsed.name, ErrorCode::LimitExceeded.legacy_name());

        // upgrade_url is dropped on the way in and never emitted on the way out: no billing
        // surface. Asserted so the omission stays a decision rather than drifting back in.
        let out = serde_json::to_string(&parsed).unwrap();
        assert!(!out.contains("upgrade_url"), "no billing surface: {out}");
        assert!(!out.contains("null"), "absent optionals are omitted: {out}");
    }

    #[test]
    fn validation_error_carries_code_path_message_issues() {
        let live = r#"{"name":"ValidationError","code":"validation_error",
            "message":"Request validation failed",
            "errors":[{"code":"custom","path":[],"message":"to, cc, or bcc must be specified"}],
            "fix":"...","docs":"https://docs.agentmail.to/errors#validation_error"}"#;
        let parsed: ErrorEnvelope = serde_json::from_str(live).unwrap();
        assert_eq!(parsed.status(), 400);
        assert_eq!(parsed.errors[0].code, "custom");
        assert!(parsed.errors[0].path.is_empty());
    }

    #[test]
    fn empty_extras_are_omitted_never_emitted_as_empty() {
        let e = ErrorEnvelope::new(ErrorCode::NotFound, "Inbox not found");
        // Check for the KEYS, not substrings: the docs URL legitimately contains "errors".
        let v: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert!(!v.contains_key("errors"), "empty errors[] must be omitted: {v:?}");
        assert!(!v.contains_key("suggestions"), "empty suggestions[] must be omitted: {v:?}");
        assert!(!v.contains_key("fix"), "absent fix must be omitted: {v:?}");
        assert_eq!(v.keys().collect::<Vec<_>>(), ["code", "docs", "message", "name"]);
    }
}
