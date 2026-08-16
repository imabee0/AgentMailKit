# amk-store — four blockers for `amk-http`, and one deletion — dispatch contract

Scope-derivation: `scripts/derive-http-prereqs.sh`, which (1) prints every `amk-store` list
function with its return type, (2) partitions the 25 first-dispatch `amk-http` operations by
whether `openapi.json` gives them a `page_token` parameter, (3) enumerates every foreign key that
can make a `DELETE` fail `23503`, (4) greps every database-error class this crate catches, (4b/4c/4d)
enumerates every **test** assertion keyed to a behaviour this dispatch changes, every use of a
constant it changes across `src` *and* `tests`, and every caller of the three functions whose
signature changes, and (5) sets the shipped key constants beside fixture 23. Its raw output is
pasted below and is the scope. **A reviewer re-runs the script; it does not read the list.**

Sections 4b–4d exist because the pre-dispatch review found two blocking misses the first version of
this script could not have found: it grepped `src/` only, and both misses were assertions in
`tests/` pinning the behaviour this dispatch overturns. **A test that asserts the old behaviour is a
site, exactly as a call site is** — that is the general lesson, and it is why the enumeration now
covers tests.

Written by the orchestrator before dispatch. The design decisions here are settled; the implementer
resolves ordinary coding detail inside them and escalates anything else.

**`amk-http` cannot start until this lands.** Every finding below was produced by checking
`.claude/contracts/amk-http.md` against `amk-store`'s real public surface — the same pre-dispatch
pass that produced the api-keys dispatch and the `inboxes::update` dispatch, at the same point in
the pipeline, for the third time. Six of the 25 operations have no paginated persistence path, two
of them return `500` where the wire requires a specific status, and one shipped constant is
contradicted by a fixture.

## Derivation output (verbatim)

```
== 1. every amk-store list function, with its return type ==
api_keys.rs: pub async fn list(pool, organization_id: &OrganizationId, scope: &KeyScope)
             -> Result<Vec<ApiKey>, StoreError>
inboxes.rs: pub async fn list(pool, organization_id: &OrganizationId, pod_id: Option<PodId>)
            -> Result<Vec<Inbox>, StoreError>
messages.rs: pub async fn list(pool, filter: &ScopeFilter, excluded_labels: &[&str], query: ListMessagesQuery)
             -> Result<Page<MessageItem>, StoreError>
organizations.rs: pub async fn list(pool)
                  -> Result<Vec<Organization>, StoreError>
pods.rs: pub async fn list(pool, organization_id: &OrganizationId)
         -> Result<Vec<Pod>, StoreError>
threads.rs: pub async fn list(pool, filter: &ScopeFilter, excluded_labels: &[&str], query: ListThreadsQuery)
            -> Result<Page<ThreadItem>, StoreError>

== 2. paginated GETs among the 25 first-dispatch operations ==
25 operations in the dispatch table; 6 carry page_token:
  GET    /v0/pods
  GET    /v0/inboxes
  GET    /v0/pods/{pod_id}/inboxes
  GET    /v0/api-keys
  GET    /v0/pods/{pod_id}/api-keys
  GET    /v0/inboxes/{inbox_id}/api-keys

== 3. foreign keys that make a DELETE fail with SQLSTATE 23503 ==
  pods.organization_id     -> organizations
  inboxes.organization_id  -> organizations
  inboxes.pod_id           -> pods
  threads.organization_id  -> organizations
  threads.pod_id           -> pods
  threads.inbox_id         -> inboxes
  messages.inbox_id        -> inboxes
  messages.organization_id -> organizations
  messages.pod_id          -> pods
  messages.thread_id       -> threads
  api_keys.organization_id -> organizations
  api_keys.pod_id          -> pods
  api_keys.inbox_id        -> inboxes

== 4. every catch of a database-error class in amk-store ==
  inboxes.rs:140:        Err(sqlx::Error::Database(db_err)) if is_inbox_pkey_violation(db_err.as_ref()) => {
  inboxes.rs:150:    db_err.is_unique_violation() && db_err.constraint() == Some("inboxes_pkey")

== 4b. every TEST assertion keyed to a behaviour this dispatch changes ==
  api_keys.rs:449:    let result = pods::delete(&pool, &org, pod).await;
  api_keys.rs:454:        matches!(result, Err(StoreError::Database(_))),
  api_keys.rs:839:/// *before* any comparison runs, surfacing as `StoreError::Database` (a 500-class error) rather
  api_keys.rs:1135:/// `Err(StoreError::Database(_))` from SQLSTATE `22021`, and a mutant that reintroduced that would
  api_keys.rs:1309:/// database error escaping as `StoreError::Database`, not merely "some Result variant".
  control_plane.rs:47:    assert!(organizations::list(&pool)
  control_plane.rs:183:/// Isolates `pods::delete`'s organization pin — a destructive cross-tenant write if dropped.
  control_plane.rs:195:        !pods::delete(&pool, &org_b, pod_a).await.unwrap(),
  control_plane.rs:203:    assert!(pods::delete(&pool, &org_a, pod_a).await.unwrap());
  control_plane.rs:512:// a `StoreError::Database`, not the uniform not-found every other unresolvable id produces. Each
  control_plane.rs:519:/// `Ok(None)`, never `Err(StoreError::Database(_))`.
  control_plane.rs:553:/// `INSERT` bind rather than a masked lookup — an ungraceful `StoreError::Database`, not a
  messages_and_threads.rs:2834:// Postgres parameter encoding (SQLSTATE 22021): a `StoreError::Database`, not the uniform
  messages_and_threads.rs:2842:/// `inbox_id` *parameter* must return `Ok(None)`, never `Err(StoreError::Database(_))`.

== 4c. every use of a constant this dispatch changes, src AND tests ==
  src/api_keys.rs:13://! `[PREFIX_TAG]` + `[SECRET_LEN]` below. A minted key never begins `am_eu_` — trivially true of a
  src/api_keys.rs:20://! first [`VISIBLE_LEN`] characters of that random portion — `[ASSUMED]` split, chosen because the
  src/api_keys.rs:69:const PREFIX_TAG: &str = "am_us_";
  src/api_keys.rs:70:/// Total length of the random portion of a minted secret (after [`PREFIX_TAG`]).
  src/api_keys.rs:71:const SECRET_LEN: usize = 32;
  src/api_keys.rs:74:const VISIBLE_LEN: usize = 8;
  src/api_keys.rs:151:/// `SECRET_LEN` alphanumeric characters from a CSPRNG. `rand::rngs::OsRng` draws directly from
  src/api_keys.rs:157:        .take(SECRET_LEN)
  src/api_keys.rs:165:    let secret = format!("{PREFIX_TAG}{random}");
  src/api_keys.rs:167:        .get(..VISIBLE_LEN)
  src/api_keys.rs:168:        .expect("invariant: SECRET_LEN (32) is always >= VISIBLE_LEN (8)");
  src/api_keys.rs:169:    let prefix = format!("{PREFIX_TAG}{visible}");
  src/api_keys.rs:174:/// short or does not carry [`PREFIX_TAG`] at all — a caller-controlled string, so this must never
  src/api_keys.rs:178:    let rest = presented.strip_prefix(PREFIX_TAG)?;
  src/api_keys.rs:179:    let visible = rest.get(..VISIBLE_LEN)?;
  src/api_keys.rs:180:    Some(format!("{PREFIX_TAG}{visible}"))
  src/api_keys.rs:545:        assert!(secret.starts_with(PREFIX_TAG));
  src/api_keys.rs:546:        assert_eq!(secret.len(), PREFIX_TAG.len() + SECRET_LEN);
  src/api_keys.rs:547:        assert!(secret[PREFIX_TAG.len()..]
  src/api_keys.rs:550:        assert!(prefix.starts_with(PREFIX_TAG));
  src/api_keys.rs:551:        assert_eq!(prefix.len(), PREFIX_TAG.len() + VISIBLE_LEN);
  src/api_keys.rs:562:        // cheap to assert so a later change to PREFIX_TAG cannot silently reintroduce it.
  src/api_keys.rs:594:            "am_us_\u{1F600}\u{1F600}", // multi-byte characters straddling the VISIBLE_LEN cut
  src/api_keys.rs:595:            "am_us_1234567",            // one short of VISIBLE_LEN
  tests/api_keys.rs:1343:    // (`PREFIX_TAG`/`VISIBLE_LEN` are private, and this test has no business hardcoding either):

== 4d. every caller of the three list functions that change signature ==
  crates/amk-store/tests/api_keys.rs:500:    let listed_a = api_keys::list(&pool, &org, &KeyScope::Pod(pod_a))
  crates/amk-store/tests/api_keys.rs:506:    let listed_b = api_keys::list(&pool, &org, &KeyScope::Pod(pod_b))
  crates/amk-store/tests/api_keys.rs:551:    let listed_a = api_keys::list(&pool, &org, &KeyScope::Inbox(inbox_a))
  crates/amk-store/tests/api_keys.rs:557:    let listed_b = api_keys::list(&pool, &org, &KeyScope::Inbox(inbox_b))
  crates/amk-store/tests/api_keys.rs:735:    let listed_a = api_keys::list(&pool, &org_a, &KeyScope::Organization)
  crates/amk-store/tests/api_keys.rs:794:    let listed = api_keys::list(&pool, &org, &KeyScope::Organization)
  crates/amk-store/tests/api_keys.rs:1245:    let listed = api_keys::list(&pool, &org, &KeyScope::Inbox(hostile))
  crates/amk-store/tests/api_keys.rs:1292:    let listed = api_keys::list(&pool, &org, &KeyScope::Inbox(inbox_a))
  crates/amk-store/tests/control_plane.rs:81:    let all = pods::list(&pool, &org).await.unwrap();
  crates/amk-store/tests/control_plane.rs:229:    let all = inboxes::list(&pool, &org, None).await.unwrap();
  crates/amk-store/tests/control_plane.rs:366:    let rows = inboxes::list(&pool, &org, None).await.unwrap();
  crates/amk-store/tests/control_plane.rs:465:/// `inboxes::list` must return the *exact* set for its organization, not merely include it:
  crates/amk-store/tests/control_plane.rs:476:    let list_a = inboxes::list(&pool, &org_a, None).await.unwrap();

== 5. the minted-key constants, against fixture 23 ==
  api_keys.rs:69:const PREFIX_TAG: &str = "am_us_";
  api_keys.rs:71:const SECRET_LEN: usize = 32;
  api_keys.rs:74:const VISIBLE_LEN: usize = 8;
  fixture 23:  prefix     "am_us_ae0c53"                    = `am_us_` + **6** lowercase-hex characters
  fixture 23:  api_key    "am_us_" + 64 lowercase-hex chars = the prefix's 6 hex, then 58 more
```

Section 4 is the one worth reading twice: **exactly one** database-error class is caught anywhere in
this crate's `src`, and it is a unique violation. Nothing catches a foreign-key violation, so every
row of section 3 is a potential `500`.

Section 4b's first two lines are the second thing to read twice —
`tests/api_keys.rs:449,454` is a test that *asserts* the defect decision 2 fixes.

## `[SPEC:*]` and `[TESTED]` citations

- `[SPEC:openapi]` — all six operations in section 2 carry `limit`, `page_token` and `ascending`.
  The envelope is `{count, limit?, next_page_token?, <resource>: []}`; `next_page_token` is
  **absent**, never `null` or `""`, on the last page.
- `[SPEC:sdk]` — the page-token internal format is unspecified and opaque to clients, so our
  encoding is free. Keep `base64(JSON)` via `amk_types::page::Cursor`, matching the *outer* shape
  observed in `reference/fixtures/04-pagination.http`.
- `[TESTED]` `reference/fixtures/22-org-mount-and-delete-semantics.txt` — `DELETE /v0/pods/{pod_id}`
  on a pod that still owns an inbox returns **409 `cannot_delete`**, full envelope, and the refusal
  is **total**: neither the pod nor the inbox is touched. In the same probe run,
  `DELETE /v0/inboxes/{inbox_id}` returned **202** with no emptiness precondition of any kind.
- `[TESTED]` `reference/fixtures/23-inbox-defaults-and-key-shape.txt` — a real minted key is
  `am_us_` + 64 lowercase-hex characters and its `prefix` is the first **6** of them.
- `[TESTED]` against the dev database, 2026-08-16 — both blocked deletes, reproduced verbatim:

  ```
  delete from pods where pod_id='1111…';
  ERROR:  23503: update or delete on table "pods" violates foreign key constraint
          "inboxes_pod_id_fkey" on table "inboxes"

  delete from inboxes where inbox_id='fk@x.test';
  ERROR:  23503: update or delete on table "inboxes" violates foreign key constraint
          "api_keys_inbox_id_fkey" on table "api_keys"
  ```

  The second one is the surprise: it needs **only** an inbox-scoped API key, and
  `POST /v0/inboxes/{inbox_id}/api-keys` followed by `DELETE /v0/inboxes/{inbox_id}` are both in the
  25. It is reachable in the first dispatch with no mail in the system at all.

`amk_types` is **frozen**. `amk_types::page::Cursor`, `ListParams` and the `page!` envelope macro
already model everything the wire needs. **Do not add a type there. Do not ask for one.**

## Writable paths (exact)

`crates/amk-store/**` and `scripts/derive-http-prereqs.sh` (the derivation script above — commit it
so a reviewer can re-run it). Nothing else. If the work requires a path outside that tree,
**STOP and report**.

## Decisions (settled — implement, do not relitigate)

### 1. Three list functions become keyset-paginated

`pods::list`, `inboxes::list` and `api_keys::list` return `Page<T>` and take a query struct, exactly
as `messages::list` and `threads::list` already do. **Copy those two functions' structure; do not
invent a second pagination idiom in the same crate.**

```rust
pub struct ListPodsQuery    { pub limit: u64, pub direction: SortDirection, pub cursor: Option<PodCursor> }
pub struct ListInboxesQuery { pub limit: u64, pub direction: SortDirection, pub cursor: Option<InboxCursor> }
pub struct ListApiKeysQuery { pub limit: u64, pub direction: SortDirection, pub cursor: Option<ApiKeyCursor> }

pods::list(pool, organization_id, query)                 -> Result<Page<Pod>,    StoreError>
inboxes::list(pool, organization_id, pod_id, query)      -> Result<Page<Inbox>,  StoreError>
api_keys::list(pool, organization_id, scope, query)      -> Result<Page<ApiKey>, StoreError>
```

Three new cursor types in `pagination.rs`, beside `MessageCursor`/`ThreadCursor`. **The exact
fields and `decode` signatures, settled — do not add, drop, or rename one:**

```rust
pub struct PodCursor    { pub created_at: DateTime<Utc>, pub pod_id: PodId }
pub struct InboxCursor  { pub created_at: DateTime<Utc>, pub inbox_id: InboxId, pub pod_id: PodId }
pub struct ApiKeyCursor { pub created_at: DateTime<Utc>, pub api_key_id: ApiKeyId,
                          pub pod_id: Option<PodId>, pub inbox_id: Option<InboxId> }

PodCursor::decode(token: &str)                          -> Result<Self, PageTokenError>
InboxCursor::decode(token: &str, pinned: Option<PodId>) -> Result<Self, PageTokenError>
ApiKeyCursor::decode(token: &str, pinned: &KeyScope)    -> Result<Self, PageTokenError>
```

JSON field names are the Rust field names verbatim, as `MessageCursor`/`ThreadCursor` already do.
Timestamps encode through the existing `encode_timestamp`/`decode_timestamp` pair — do not write a
second RFC-3339 formatter.

**Keyset is two columns, not three** — unlike `MessageCursor`. `pod_id`, `inbox_id` and
`api_key_id` are each their table's primary key, so `(created_at, <pk>)` is already a total order;
`MessageCursor` needs `inbox_id` in the tiebreak only because a Message-ID is unique per *inbox*,
not per table. Do not copy that third column, and do not drop the tiebreak either: `created_at` is
`timestamp(3)` and two rows created in the same millisecond would make the walk skip one.

**A cursor carries exactly the coordinates a narrower mount could differ on — no more.** That is
the rule `MessageCursor` already follows, and it is why none of the three carries
`organization_id`: neither existing cursor does, one credential resolves to exactly one
organization, and the query's own `WHERE organization_id = $1` pin is what isolates tenants. Adding
an org check would be a new check with no precedent and nothing it can catch. So:

- `PodCursor` — `GET /v0/pods` is its only mount, so there is nothing to pin and `decode` takes no
  pinned argument. It also has **no free-text field**, so it needs no `has_forbidden_byte` check;
  a NUL in `pod_id` fails the UUID parse as `WrongType` first. Both absences are decisions, not
  omissions — say so in the doc comment.
- `InboxCursor` — two mounts (org, pod). `pinned: None` is the org mount and accepts any token;
  `Some(p)` requires `cursor.pod_id == p`, else `WrongScope`. `inboxes.pod_id` is `NOT NULL`, so
  the field is always knowable. This is `check_inbox_scope`'s exact shape, one level up.
- `ApiKeyCursor` — three mounts, and the subtlety worth stating: the *mount* is not the key's own
  scope. `KeyScope::Organization` lists pod- and inbox-scoped keys too (see `KeyScope`'s own doc),
  so the cursor records the **mount it was minted at**, encoded as the same `(Option<PodId>,
  Option<InboxId>)` pair `scope_params` already collapses `KeyScope` into. `decode` rebuilds that
  pair from `pinned` and requires equality — `Organization` is `(None, None)`, a real checkable
  value, not "no coordinate". Compare the inbox half with `eq_normalized`, never `==` (fixture 18),
  and apply `has_forbidden_byte` to it, exactly as the two existing decoders do to theirs.

`WrongScope` is not decoration: without it a token minted at `GET /v0/pods/A/inboxes` replayed at
`GET /v0/pods/B/inboxes` silently resumes mid-list of a *different* pod — not a disclosure, since
the query pins its own scope, but a wrong page returned as if it were right.

**The `Page<T>` → `{count, limit?, next_page_token?, <resource>: []}` envelope conversion is
`amk-http`'s, not yours.** `Page` is this crate's shape and stays that way; `amk_types::page`'s
macro builds the wire envelope. Do not construct a wire envelope here.

`ascending` maps to `SortDirection` exactly as it already does for messages and threads: this crate
takes a resolved `SortDirection`, and turning `Option<bool>` into one is the caller's job.

Reuse, do not re-derive:

- `SortDirection` and **two fixed SQL literals per function**, selected by a `match`. The direction
  is never formatted into query text. This is not style — it is the rule that keeps this crate free
  of string-built SQL.
- The `limit == 0` early return (`Page { items: vec![], next: None }`).
- `let fetch_limit = query.limit.saturating_add(1).min(i64::MAX as u64) as i64;` and
  `has_more = rows.len() as u64 > query.limit`. Read `messages::list`'s comment on why: `limit` is
  an unclamped `u64` straight off a query string, and a wrapped `LIMIT` renders as an empty page
  indistinguishable from an empty mailbox.
- The existing scope pins, verbatim: `($n::uuid IS NULL OR pod_id = $n)` for `inboxes::list`, and
  `api_keys::scope_params` with its NUL guard **ahead of** the helper, never inside it — that guard
  is the difference between an inbox-scoped request missing and an inbox-scoped request silently
  widening to the whole organization.

### 2. `pods::delete` gains a `cannot_delete` path

`pods::delete` returns `Result<bool, StoreError>` today and cannot express the observed refusal. Add
`StoreError::PodNotEmpty` and return it when the `DELETE` fails with a foreign-key violation whose
constraint names `pods` as the referenced table. Pattern to copy, from `inboxes.rs:140-151`:

```rust
Err(sqlx::Error::Database(db_err)) if is_pod_reference_violation(db_err.as_ref()) => {
    return Err(StoreError::PodNotEmpty);
}
```

```rust
fn is_pod_reference_violation(db_err: &dyn sqlx::error::DatabaseError) -> bool {
    db_err.is_foreign_key_violation()
        && matches!(db_err.constraint(),
            Some("inboxes_pod_id_fkey" | "threads_pod_id_fkey"
                 | "messages_pod_id_fkey" | "api_keys_pod_id_fkey"))
}
```

**Match the constraint name, not just the SQLSTATE.** `23503` on this statement could also come
from a future constraint that means something else entirely, and a bare `is_foreign_key_violation()`
would rename that error `PodNotEmpty` and hand `amk-http` a `409` for it. The four names are the
complete set from section 3 of the derivation, and they are the live names — verified with
`pg_constraint`, not guessed from Postgres' naming convention.

`amk-http` maps `PodNotEmpty` to `ErrorCode::CannotDelete`, which `ErrorCode::status()` already
returns `409` for (commit `2318e9c`). **This crate constructs no wire error** — it exposes the
distinguishable variant and stops, exactly as `InboxAlreadyExists` does.

**`crates/amk-store/tests/api_keys.rs:429-462` currently asserts the defect.**
`deleting_a_pod_that_owns_keys_is_rejected_by_the_declared_fk_behaviour` pins
`Err(StoreError::Database(_))` and carries a doc comment reading *"the declared FK behaviour is the
default (no ON DELETE clause, same as every other table in this crate)"* — which stops being true
in this dispatch, for `inboxes` in decision 3, and stops being the observable outcome here.
Rewrite the assertion to `Err(StoreError::PodNotEmpty)` **and** the comment; leave its second
assertion (the pod survives) exactly as it is, because that half is what fixture 22's *total*
refusal requires. Found by the pre-dispatch review, in section 4b — not by reading this contract.

### 3. `inboxes::delete` cascades; `pods::delete` does not

These two are deliberately opposite answers and the difference is derived, not stylistic. Fixture 22
shows AgentMail *does* refuse a delete when it wants to — that is what `cannot_delete` on the pod
is — and in the same probe run the inbox delete returned `202` unconditionally. Under the schema as
it stands, an inbox that has ever received a message or been given a scoped key is permanently
undeletable, which contradicts that `202` and makes the main path unreachable.

Migration `0008`: add `ON DELETE CASCADE` to the three foreign keys referencing `inboxes`
(`threads_inbox_id_fkey`, `messages_inbox_id_fkey`, `api_keys_inbox_id_fkey`) and to
`messages_thread_id_fkey`. Leave **every** foreign key referencing `pods` at its default
(`NO ACTION`) — that is what keeps decision 2 working: a pod delete must still trip
`inboxes_pod_id_fkey`.

`messages_thread_id_fkey` is in the list because the inbox cascade deletes `threads` rows whose
`messages` are being deleted by a *different* cascade in the same statement, and the order Postgres
runs those referential actions in is not something to settle from an armchair. So it was measured.
**`[TESTED]` against the dev database, 2026-08-16, inside a rolled-back transaction** — this is the
DDL, verbatim, and it is what `0008` must contain:

```sql
alter table threads  drop constraint threads_inbox_id_fkey,
  add constraint threads_inbox_id_fkey   foreign key (inbox_id)  references inboxes (inbox_id)  on delete cascade;
alter table messages drop constraint messages_inbox_id_fkey,
  add constraint messages_inbox_id_fkey  foreign key (inbox_id)  references inboxes (inbox_id)  on delete cascade;
alter table api_keys drop constraint api_keys_inbox_id_fkey,
  add constraint api_keys_inbox_id_fkey  foreign key (inbox_id)  references inboxes (inbox_id)  on delete cascade;
alter table messages drop constraint messages_thread_id_fkey,
  add constraint messages_thread_id_fkey foreign key (thread_id) references threads (thread_id) on delete cascade;
```

With one org / pod / inbox / thread / message / inbox-scoped key seeded behind it:

```
delete from pods ...     ->  ERROR 23503, constraint "inboxes_pod_id_fkey"     <- the refusal survives
delete from inboxes ...  ->  DELETE 1, and afterwards:
                             inboxes 0 | threads 0 | messages 0 | api_keys 0 | pods 1
```

No ordering problem: the two cascades run in the same statement without `messages_thread_id_fkey`
firing on rows that are themselves being deleted. The assigned test below re-establishes this from
Rust; if the cascade set is ever narrowed, it fails with a `23503`.

Do it at the database, not in a Rust transaction: a cascade is atomic by construction and cannot be
forgotten by a future caller of `inboxes::delete`.

### 4. The minted-key constants are wrong, and `prefix` is a wire field

`SECRET_LEN` → **64**, `VISIBLE_LEN` → **6**, alphabet → **lowercase hex**. Mint by drawing **32
bytes** from `rand::rngs::OsRng` and hex-encoding them lowercase — 256 bits exactly, and no
modulo-bias question to answer. Write the hex by hand (`write!(s, "{b:02x}")`); **do not add a
dependency** for four characters of format string.

Both constants were tagged `[ASSUMED]` by the api-keys contract, correctly: the only prior evidence
was fixture 05's rejected `am_us_0000…0000`, whose length showed what the gateway accepts as
well-formed, not what it mints. Fixture 23 supersedes that with an observation, and `prefix` is
returned in **every** `ApiKey` response — it is a pinned wire field, not an internal detail. Leave
the `am_eu_` assertion exactly as it is.

**Section 4c is the scope of this change and it is 24 lines long, not two.** Three of those sites
are hardcoded values that will assert something *false* under the new constants, so they are
corrections, not mechanical renames:

- `src/api_keys.rs:168` — `.expect("invariant: SECRET_LEN (32) is always >= VISIBLE_LEN (8)")`
  embeds both numbers in a string literal. `cargo-mutants` does not mutate string literals, and a
  wrong `expect` message is the kind of thing a reader trusts. Restate it with the new numbers.
- `src/api_keys.rs:599` — `assert_eq!(candidate_prefix("am_us_1234567"), None, "too short to have
  a prefix at all")`. Seven characters past the tag is **no longer too short** at `VISIBLE_LEN = 6`;
  this now yields `Some("am_us_123456")`. The "one short" case becomes `"am_us_12345"`.
- `src/api_keys.rs:600-602` — `assert_eq!(candidate_prefix("am_us_12345678rest-of-the-secret"),
  Some("am_us_12345678"))` now yields `Some("am_us_123456")`.

The module doc (`src/api_keys.rs:9-20`) states the 32/8 shape as the observed one, citing fixture
05's *rejected* key. Rewrite it to cite fixture 23 and say what that fixture actually shows.

Both `candidate_prefix` assertions were found by the pre-dispatch review, in section 4c — the first
version of the derivation script grepped `src/` for *error catches* and never for *uses of the
constants being changed*, so neither could have surfaced from it. This is the same failure shape as
the id-safety dispatch's five missing sites, caught one stage earlier this time.

**Six hex characters is 16.7M prefixes and `api_keys_prefix_idx` is `UNIQUE`, so a mint can now
collide.** At ten thousand keys a collision is likelier than not over the deployment's life. It is
not a failure — it is a redraw: loop the mint up to `MINT_ATTEMPTS` (4) times, treating a unique
violation on `api_keys_prefix_idx` (and only that constraint) as "draw again". Four consecutive
collisions at any realistic key count is a probability with thirteen zeros after the decimal point;
exhausting them surfaces as the underlying error, unmapped. Do **not** solve this by dropping the
`UNIQUE` index and verifying against every matching row — that moves work onto `authenticate`, the
one function in this crate whose timing behaviour five review rounds have hardened, in exchange for
nothing.

### 5. `organizations::list` is deleted

It takes no credential and returns every organization in the deployment. It has no wire route —
`GET /v0/organizations` returns *the* organization for the authenticated key and calls
`organizations::get` — and it has exactly **one** call site anywhere in the workspace,
`tests/control_plane.rs:47` (section 4d). The `amk-http` contract currently forbids calling it in
prose, which is the "one obligation recorded in two places" shape this project has already been
bitten by. Delete the function; rewrite that one assertion against `organizations::get`. A function
that does not exist cannot be reached for.

`.claude/contracts/amk-http.md:272-275` carries the prose prohibition and goes stale the moment this
lands. **The orchestrator rewrites it at merge — do not touch that file.**

`organizations::delete` stays: it also has no wire route, but it is scoped to one id and the tests
legitimately use it for cleanup.

## Assigned edge cases (write the test before the code it targets)

Pagination, once per newly-paginated function — three functions, so each bullet is three tests
unless it says otherwise:

- A full walk: seed 5 rows, page with `limit: 2`, follow `next` to exhaustion, assert the union is
  all 5 with **no duplicate and no omission**, and that the final page's `next` is `None`.
- The same walk with `SortDirection::Descending` returns the exact reverse order.
- `limit: 0` → empty page, `next: None`, and **no query runs** that could error.
- `limit: u64::MAX` → every row, one page, `next: None`, no panic and no overflow.
- Two rows sharing a `created_at` to the millisecond are both returned exactly once across the walk
  — the test that fails if the primary-key tiebreak is dropped.
- A cursor whose scope coordinate disagrees with the request's → `WrongScope`, asserted on the
  variant, not `is_err()`. For `InboxCursor`, specifically a pod-A token replayed against pod B.
- A cursor carrying a NUL in `organization_id`, and (for `InboxCursor`) in `inbox_id` →
  `ForbiddenByte(<that field>)`.
- A mixed-case `inbox_id` in an `InboxCursor` resolves the same inbox (fixture 18).
- `api_keys::list` at each of the three `KeyScope` mounts paginates within that mount only — a
  pod-scoped walk never returns another pod's key on any page, including the last.

Deletes:

- `pods::delete` on a pod owning an inbox → `Err(StoreError::PodNotEmpty)`, **and the inbox row and
  the pod row are both still present afterwards**. Assert the rows, not only the error: fixture 22's
  refusal is total, and a partial delete that then errors would pass an error-only assertion.
- One such test per referencing table — a pod owning only a thread, only a message, only an api-key
  — so the four-name constraint match is pinned in all four directions. A single-case test would
  survive deleting three of the four names.
- `pods::delete` on an empty pod → `Ok(true)`. On an absent pod → `Ok(false)`, never `PodNotEmpty`.
- `inboxes::delete` on an inbox holding a thread, a message **and** an inbox-scoped api-key →
  `Ok(true)`, with all four rows gone afterwards. This is the test that settles the cascade set.
- `inboxes::delete` in pod A for pod B's inbox → `Ok(false)` **and B's rows untouched**, including
  its api-keys: a scope miss that cascades is the worst possible version of this defect.

Keys:

- The existing `minted_key_matches_the_observed_shape` asserts 64 hex characters, a 6-character
  visible portion, and that every character is `[0-9a-f]` — citing fixture 23. A test that only
  checks lengths would pass on the current URL-safe alphabet.
- `candidate_prefix` on hostile input still never panics: add `"am_us_12345"` (one short of the new
  `VISIBLE_LEN`) and a multi-byte character straddling the 6-character cut.
- The prefix-collision predicate, tested against a **real** error: insert two rows with the same
  `prefix` by raw SQL, catch the second failure, assert the predicate returns `true` for it and
  `false` for the `inboxes_pkey` unique violation.
- `authenticate` is unchanged. Its timing test must still exist and must not be `#[ignore]`d — the
  ledger checks this, but check it yourself before reporting.

## Prohibitions

- No `mail_parser::`/`mail_auth::`/`mail_send::`/`mail_builder::`/`smtp_proto::` type in any public
  signature or re-export. No JMAP, Sieve, RocksDB, or mailbox-role concept.
- Do not edit `amk-types`, `amk-core`, the plan, any contract file, or `scripts/hooks/**`.
- Do not add a dependency. Do not add a wire type, an error envelope, or an HTTP status — this
  crate exposes distinguishable variants and nothing else.
- Do not change `authenticate`, `verify_secret`, or the argon2 parameters.
- Do not edit an existing migration file. `0008` is a new file.
- If something you need does not exist, **STOP and report**. Do not add a field that obviously
  belongs.
- Do not commit `.amk-task.md` or `.amk-scope`.

## Reporting

Report the command you ran and its actual output: `cargo test -p amk-store`, `./scripts/check.sh`,
and a **two-directional** mutation table. `cargo-mutants` does not mutate string literals, so mutate
by hand — the four constraint names, the two SQL literals per paginated function, and the new
constants. Every guard gets both directions: delete it (must kill a test) **and** widen it
(`is_some_and(pred)` → `is_some()`, `matches!(c, Some("a"|"b"|"c"|"d"))` → `c.is_some()`,
`any(pred)` → `!is_empty()`) — which must also kill a test. A deletion-only pass is structurally
blind to an over-broad guard; that is how a live mutant survived twenty mutations two dispatches
ago. Mutate the pagination too: dropping the primary-key tiebreak from an `ORDER BY`, and dropping
the `+1` from `fetch_limit`, must each kill a test. Name anything you did not do and why.
