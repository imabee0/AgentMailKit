"""Custom schemathesis checks: AgentMailKit's own wire invariants, applied to every response.

Loaded via `SCHEMATHESIS_HOOKS=conformance.schemathesis_checks`.

WHY THESE AND NOT THE STOCK SET ALONE. Schemathesis's built-in checks measure a server against its
OpenAPI document. Ours is `reference/openapi.json` — AgentMail's own spec — and this project has
now caught that document being wrong four separate times against live captures (system labels,
DELETE statuses 0-for-3, `Organization`'s field count, `ApiKeyPermissions`' flag count). So the
spec is a good oracle for *shapes* and a poor one for *statuses*, and `status_code_conformance` is
excluded at the call site with that reason recorded.

What the spec cannot express at all is the set of rules this project states as facts in CLAUDE.md
and pins with fixtures. Those are the checks below. Their value is that a fuzzer reaches states no
hand-written test enumerates — a malformed page token, a path segment that is a bare `@`, a body
whose one required field is a 400-character control string — and these invariants must hold in
every one of them, not just on the happy paths the integration suite walks.
"""
import os
import re

import schemathesis


@schemathesis.hook
def before_call(ctx, case, **kwargs):
    """Present the credential, taken from the environment — NEVER from the command line.

    Schemathesis takes headers with `-H`, and `-H "Authorization: Bearer <key>"` writes the key
    into `/proc/<pid>/cmdline`, which is world-readable for the entire duration of the run. Any
    local process can read it without any privilege at all; this was found by running `pgrep -af`
    against this project's own gate and watching a live key print. Every other secret path in this
    repository passes by reference through the environment, and so does this one.
    """
    if case.headers is None:
        case.headers = {}
    case.headers["Authorization"] = "Bearer " + os.environ["AMK_KEY"]

# `Timestamp` is wire-exact: RFC 3339, exactly three fractional digits, `Z`.
TIMESTAMP = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")
# Keys whose values are timestamps. Derived from the shapes amk-types emits, not guessed: every
# timestamp field in the P1 surface is `<something>_at`, and events add a bare `timestamp`.
TIMESTAMP_KEY = re.compile(r"(^|_)(at|timestamp)$")

# The full app-level envelope's required members. `fix`, `docs`, and the per-code extras
# (`errors[]`, `suggestions[]`, `resource`/`limit`) are optional and deliberately not required here.
ENVELOPE_REQUIRED = {"name", "code", "message"}


def _json(response):
    """Parsed JSON body, or None when the response has no JSON body to inspect.

    `Response.json` is a METHOD here, not a property — reading it without calling it yields a bound
    method that is neither dict nor list, so every check downstream silently sees "not an object".
    The first run of this module did exactly that: two checks passed everything because `_walk` on a
    method yields nothing, and the third failed everything with "body is method, not an object".
    `headers` is `dict[str, list[str]]`, lower-cased by the constructor.
    """
    if not response.content:
        return None
    if "json" not in (response.headers.get("content-type") or [""])[0]:
        return None
    try:
        return response.json()
    except Exception:
        return None


def _walk(node, path="$"):
    """Yield (json-pointer-ish path, key, value) for every member of every object in the body."""
    if isinstance(node, dict):
        for k, v in node.items():
            yield f"{path}.{k}", k, v
            yield from _walk(v, f"{path}.{k}")
    elif isinstance(node, list):
        for i, v in enumerate(node):
            yield from _walk(v, f"{path}[{i}]")


@schemathesis.check
def optionals_are_omitted_never_null(ctx, response, case):
    """An absent optional is omitted from the wire. Never `null`, never `""`.

    [SPEC:fixtures 01, 03, 04, 17, 22, 23 — every live capture omits what it does not have.] The
    live API never emits a JSON null in this surface, and both official SDKs model absence as an
    optional rather than as a nullable. A `null` we emit would deserialize into one client as
    `None` and the other as `undefined` and would diff against the reference in fixture 25 — but
    only on a path that fixture happens to exercise. This applies the rule everywhere the fuzzer
    reaches, which is the point of running a fuzzer at all.
    """
    body = _json(response)
    if body is None:
        return None
    offenders = [p for p, _, v in _walk(body) if v is None]
    if offenders:
        raise AssertionError(
            "optional emitted as JSON null instead of being omitted: "
            + ", ".join(sorted(offenders)[:8])
        )


@schemathesis.check
def timestamps_are_wire_exact(ctx, response, case):
    """Every timestamp is RFC 3339 with exactly three fractional digits and a `Z`.

    [SPEC:CLAUDE.md contract facts; fixtures 01/03/22/23.] Serde's default for `chrono` renders a
    whole-second instant WITHOUT the fractional part, so this is the one formatting rule in the
    project that a correct-looking implementation breaks roughly one time in a thousand — exactly
    the frequency a fuzzer finds and a fixed-fixture test does not. The Node SDK's `Date` coercion
    accepts far more than this, so it cannot catch the divergence either.
    """
    body = _json(response)
    if body is None:
        return None
    bad = [
        f"{p}={v!r}"
        for p, k, v in _walk(body)
        if TIMESTAMP_KEY.search(k) and isinstance(v, str) and not TIMESTAMP.match(v)
    ]
    if bad:
        raise AssertionError("timestamp is not RFC 3339 with three fractional digits and Z: "
                             + ", ".join(sorted(bad)[:8]))


@schemathesis.check
def error_shape_is_one_of_the_two(ctx, response, case):
    """Every error is EITHER the bare auth-layer body OR the full app envelope. Never a third thing.

    [SPEC:fixture 05-error-catalog.http.] The asymmetry is real and observed: auth-layer failures
    return a bare `{"message": ...}` at 401/403 — including for a well-formed-but-unknown key —
    while app-level failures return `{name, code, message, ...}`. Clients branch on `code`, so a
    body that is neither shape is unbranchable, and a framework-generated body (axum's plaintext
    400 on a `Path<Uuid>` rejection is the one this project already hit) is exactly that.

    The bare shape is admissible ONLY at 401/403, because that is the only place the live API emits
    it. A bare `{"message"}` at 404 would be a new third contract.
    """
    if response.status_code < 400:
        return None
    body = _json(response)
    if body is None:
        raise AssertionError(
            f"{response.status_code} response is not a JSON object "
            f"(content-type={response.headers.get('content-type')}); "
            f"every error in this surface is one of the two JSON shapes"
        )
    if not isinstance(body, dict):
        raise AssertionError(f"{response.status_code} body is {type(body).__name__}, not an object")

    keys = set(body)
    if keys == {"message"}:
        if response.status_code not in (401, 403):
            raise AssertionError(
                f"bare auth-layer body at {response.status_code}; the bare shape is observed only "
                f"at 401/403 — an app-level failure owes the full envelope"
            )
        return None
    missing = ENVELOPE_REQUIRED - keys
    if missing:
        raise AssertionError(
            f"{response.status_code} body is neither the bare auth shape nor the full envelope: "
            f"missing {sorted(missing)}, has {sorted(keys)}"
        )
