# AgentMailKit — self-hosted, 1:1 API-compatible AgentMail clone (Rust, k3s)

## Evidence legend
- `[SPEC:openapi]` — from `openapi.json` (237,737B, downloaded this session from https://docs.agentmail.to/openapi.json to scratchpad) — 82 paths, 242 schemas.
- `[SPEC:sdk]` — read from the cloned official SDK source (scratchpad `agentmail/agentmail-python` v0.5.9, `agentmail-node` v0.5.19; extracts `types_dump.txt`, `endpoints.txt`).
- `[SPEC:docs <page>]` — from docs.agentmail.to pages fetched this session.
- `[SPEC:repo <name>]` — read from AgentMail's public GitHub repos this session (agentmail-mcp, agentmail-toolkit, agentmail-skills, agentmail-cli, agentmail-examples, agentmail-schemas).
- `[SPEC:ops <path>]` — read from local repos (`appsynergy-rs/ops`, `nuc-k3s/dns`, `mail-deploy-20260724`).
- `[TESTED]` — I executed it this session and observed the result; what was done is stated.
- `[INFERRED]` — reasoned from partial evidence; reasoning stated.
- `[ASSUMED]` — design choice, not externally derived.
- `[TODO-VERIFY]` — plausible, must be confirmed before being relied on.

## Context

AgentMail (agentmail.to) is a hosted email API for AI agents. Goal: an open-source, self-hosted, **strict 1:1 drop-in clone with zero billing surface** — official SDKs/CLI/toolkit/skills work by changing only the base URL — in Rust, deployed correctly on the user's OVH k3s cluster, replacing the existing Stalwart deployment. Multi-tenant model (org → pod → inbox), single default tenant at first boot. Every open design question defaults to "what AgentMail does"; their public artifacts are the spec. User decisions (fixed): drop-in clone, Rust, mirror their pipeline without adopting a wholesale third-party mail server, all features eventually, MCP + connectors, no billing/Stripe/x402/Clerk surface, evidence-tagged planning per these rules.

## Gathered evidence

### Contract
- Full API surface: 82 paths / 242 schemas / 10 event types; triple-scoped mounts (org / `pods/{pod_id}` / `inboxes/{inbox_id}`) sharing types `[SPEC:openapi]`, corroborated by 128 SDK endpoints `[SPEC:sdk]`. Drafts mutate only at inbox scope; pods have no PATCH `[SPEC:sdk]`.
- Auth `Authorization: Bearer`; env `AGENTMAIL_API_KEY`; node SDK routes `am_eu_`-prefixed keys to the EU host `[SPEC:sdk environments.ts, Client.ts:80]`. Hosts: `api.agentmail.to`, `wss://ws.agentmail.to/v0` (+x402/mpp/eu variants — out of scope, no billing) `[SPEC:sdk]`.
- IDs: `inbox_id` = email address `[SPEC:openapi]`; `pod_id`/`thread_id`/`attachment_id` = UUID, `domain_id` = domain name `[SPEC:repo agentmail-schemas]` — schemas repo is stale (last sync ~2026-03), so `[TODO-VERIFY]` against a live account. `message_id` shown twice on docs.agentmail.to/attachments as an RFC 5322 Message-ID **with angle brackets** (`<abc123@agentmail.to>`, `<def456@agentmail.to>`) `[SPEC:docs attachments]` — implies header-derived not minted, and URL-encoding of `<`/`>`/`@` in path segments must match exactly; confirm against a real message in P-1 item 3. Event ids `evt_...`, webhook secrets `whsec_...` `[SPEC:docs webhook-verification]`.
- Pagination envelope `{count, limit?, next_page_token?, <resource>: []}`; `before/after/ascending`, `labels[]`, visibility flags default false; thread/message substring filters AND-semantics `[SPEC:sdk]`. Filtered list `limit` capped at 100 `[SPEC:repo agentmail-cli help text]`. Page-token internal format: unspecified, opaque to clients `[SPEC:sdk]` — our encoding is free `[ASSUMED]`.
- Errors: app-level envelope `{name, code, message, fix?, docs?}` **plus, on `validation_error`, an `errors` array of `{path, message}`** `[SPEC:docs errors, user-verified]`. **Clients must branch on `code`** — `name`/`message` deliberately keep legacy values (a permission denial still reads `Forbidden`) `[SPEC:docs errors]`. `[TESTED]` live probes (curl, this session): no auth → `401 {"message":"Unauthorized"}`; key `am_us_invalid_test` → `403 {"message":"Forbidden"}` (CloudFront + API Gateway headers observed); unknown path or wrong method → 404 full envelope `code:"not_found"` (no 405). **Caveat `[INFERRED]`:** the bare 403 was probably the gateway rejecting a malformed key before the app; docs imply a well-formed-but-unknown key returns a full envelope with `code: unknown_api_key` — re-probe with a realistic-format invalid key in P-1 item 5; this changes what P0 implements. Inbox-name collision: docs place `resource_taken` (409, "value already in use", inbox username as the example) `[SPEC:docs errors]` vs the SDK-derived 422 guess `[INFERRED]` — collision code is `[TODO-VERIFY]` in P-1 item 5. Block lists: entries auto-added from bounces/complaints/unsubscribes are `read_only` and undeletable via API — clone must model the auto-add path, not just the flag `[SPEC:docs errors, user-verified]`.
- Idempotency: `client_id` on creates (replay → original resource); `Idempotency-Key` header on sends — org-scoped, mismatch → 409, empty → 400, TTL "24h after send completes" `[SPEC:docs idempotency]`.
- Events: 10 types; envelope `{type:"event", event_type, event_id, <payload key>}` with per-type payload key (`message`+`thread`/`send`/`delivery`/`bounce`/`complaint`/`reject`/`domain`) `[SPEC:openapi webhooks + sdk]`. Spam/blocked/unauthenticated variants are opt-in and replace `message.received` `[SPEC:docs events]`. Webhook wire = Svix format: `svix-id/timestamp/signature`, HMAC-SHA256 over `{id}.{timestamp}.{body}`, retries reuse `svix-id` `[SPEC:docs webhook-verification]`. Retry schedule `[SPEC:svix — docs.svix.com/retries + svix-webhooks/server/svix-server/config.default.toml]`: immediate, then 5s, 5m, 30m, 2h, 5h, 10h, 10h (OSS default `retry_schedule = [5,300,1800,7200,18000,36000,36000]`), each period after the preceding failure; **2xx must arrive within 15s** or the attempt fails; 3xx counts as failure; schedule exhaustion marks the message Failed and fires `message.attempt.exhausted`; an endpoint failing 5 straight days is disabled with an `EndpointDisabledEvent`. Only remaining unknown: whether AgentMail overrides these defaults — the P-1 item-7 live probe answers exactly that.
- WS protocol: `Subscribe{type:"subscribe", event_types?, inbox_ids?, pod_ids?}` → `Subscribed` → typed events; `Error{type:"error", name, message}`; auth via `?api_key=` or header `[SPEC:sdk websockets]`. `[TESTED]` unauthenticated upgrade → 403 pre-upgrade (AWS API Gateway WebSocket error body observed).
- Behavior details: reply reuses parent subject with `Re:` (no subject param) `[SPEC:sdk ReplyToMessageRequest]`; `reply_all` mutually exclusive with to/cc/bcc; SendAttachment = exactly one of `content`|`url` `[SPEC:repo agentmail-toolkit schema refines]` (the attachments docs page saying `content` is required contradicts this and is stale — the error reference documents the unfetchable-URL failure case; trust the toolkit `[SPEC, user-verified]`); thread delete = soft-trash then permanent purge on 2nd call `[SPEC:repo agentmail-mcp tool annotations]`; block over allow priority `[SPEC:repo agentmail-toolkit]`; system labels immutable via PATCH `[SPEC:repo agentmail-mcp]` — **now `[TESTED]` and narrowed: exactly `{sent, received, bounced, scheduled}` on messages AND threads, see B6**; labels observed `received/unread/sent/scheduled/bounced/complained` + the four restricted `[TESTED fixture 19 + SPEC:repo agentmail-examples]`; `download_url`+`expires_at` on raw/attachments `[SPEC:openapi]` with ~1h expiry `[SPEC:repo agentmail-toolkit]`, unconfirmed live `[TODO-VERIFY]` — and the attachments docs page instead documents raw bytes returned inline `[SPEC:docs, user-verified]`; likely reconciliation `[INFERRED]`: bytes inline below the ≈5.95MB threshold, URL above — P-1 item 6 probes both a small and a large attachment to settle whether the URL path is the main path or the edge case; response-size inline limit ≈5.95MB with URL fallback `[SPEC:repo agentmail-toolkit citing agentmail-api limits.ts]`; drafts `send_at`/`send_status ∈ scheduled|sending|failed`, sent draft deleted `[SPEC:sdk + repo agentmail-mcp]`; viruses rejected at gateway, spam stored+hidden, auth-failure → `unauthenticated` `[SPEC:docs spam-virus-detection]`; metadata merge semantics (key→null deletes) `[SPEC:repo agentmail-mcp update_inbox]`.
- Permissions: **36** optional bool flags (~~34~~ — corrected 2026-08-15 after two independent reviewers counted the catalog: `openapi.json` `type_api-keys:ApiKeyPermissions` has 36 properties, matching `reference/types_dump.txt`; the code was right and this line was stale), whitelist mode, effective = scope ∩ whitelist, child ⊄ parent, label-denial filters as `not_found` `[SPEC:docs permissions + sdk ApiKeyPermissions]`. AgentID P-256 public-key credentials `[SPEC:sdk]`. Agent signup + 6-digit OTP; unverified caps 1 inbox / 10 sends/day, OTP 24h TTL, ≤10 attempts `[SPEC:repo agentmail-mcp agent_verify description]`.
- IMAP/SMTP passthrough: IMAP :993 TLS, user=inbox email, pass=API key, IDLE, folders INBOX/Sent/Trash/Spam + empty Drafts; SMTP :465/:587 `[SPEC:docs imap-smtp]`. `[TESTED]` their :465 answered `421 ... You talk too soon` to pipelined EHLO (greet-pause anti-abuse; banner exposed an EC2 hostname).
- Their backend: inbound/outbound = AWS SES (MX `inbound-smtp.us-east-1.amazonaws.com`, SPF `include:amazonses.com` — DNS lookups run this session `[TESTED by subagent]`); Svix webhooks, Talon reply extraction (93.8% claimed), Stripe billing `[SPEC:docs + blog agentmail-vs-amazon-ses]`. They do NOT run a general-purpose mail server `[INFERRED]` from the above — validates our libraries-not-mailserver approach.
- Ecosystem: MCP = toolkit's MCP adapter behind Streamable HTTP; 26-tool manifest (24 usable + 2 oauthOnly org tools; `auth_me` filtered); bridges are pure stdio→HTTP proxies; auth precedence `?apiKey` → `x-api-key` → `Bearer am_...` → env `[SPEC:repo agentmail-mcp]`. `[TESTED]` live `/mcp` unauthenticated → 401 + `www-authenticate: Bearer resource_metadata=.../.well-known/oauth-protected-resource/mcp` (Express/Clerk headers). Toolkit adapters (Vercel AI SDK/MCP/LangChain/clawdbot; python: OpenAI Agents/LangChain/LiveKit) wrap the SDK client `[SPEC:repo agentmail-toolkit]`. Skills = SKILL.md + references, 8 active, call MCP tool names only `[SPEC:repo agentmail-skills]`. CLI is Stainless-generated with `--base-url` `[SPEC:repo agentmail-cli]`. Licenses: python/node/mcp/toolkit-npm MIT, cli/go Apache-2.0 `[SPEC:sdk pyproject/package.json + repo]`.

### Current deployment (to be replaced)
- Live: ns `mail` in `appsynergy-rs/ops/k3s/` — Stalwart + Caddy sidecar, `replicas:1 Recreate`, public IP `64.112.14.64` via Multus NAD on `br-pub`; no ingress controller/LB/cert-manager; config+DKIM keys+RocksDB un-versioned inside `stalwart-data` PV; PSA baseline `[SPEC:ops k3s/36-mail.yaml, 35-mail-storage.yaml, 00-namespaces.yaml]`. `[TESTED]` (ssh root@144.217.66.212, read-only, this session): Stalwart image `v0.15.5` upstream running → WebAuthn fork features NOT in production.
- Networking: `[TESTED]` live server kernel `7.1.5-3-appsynergy-server-skylake` has `br_netfilter` loaded, `xt_physdev` loaded (26 refs), `/proc/sys/net/bridge/*` present; k3s config comments state `masquerade-all` already removed; `disable: [traefik, servicelb]`. The `ops/DEPLOY.md` "kernel lacks bridge-netfilter" note is **stale**. `[TESTED]` outbound port 25 egress OPEN (STARTTLS reached gmail-smtp-in from the server). `[TESTED]` no MetalLB/kube-vip/cert-manager/CNPG/Postgres anywhere in the cluster; no LoadBalancer Services.
- Domains: `appsynergy.io`, `imabee.com`, `imabee.ca`, `imabee.cloud` — MX `m.appsynergy.io` (A 64.112.14.64), SPF `ip4:64.112.14.64 -all`, DMARC p=reject, shared DKIM selector `s20260410`, PTR `.64 → m.appsynergy.io` `[SPEC:ops nuc-k3s/dns/zones/*.json]`. Live DKIM key location: per P-1 item 10 observation (see P-1 status) — inside Stalwart's RocksDB store, NOT the stale `etc/dkim/*` files the ops docs pointed at.
- Dependents on Stalwart: Bulwark webmail (JMAP), admin SPA, IMAP/submission clients, ManageSieve, plus the config-verified dependents list per P-1 item 12 (Bulwark + inert CP relay per item 11; keel/combly/mellify undeployed templates).
- Host firewall is declarative: `ops/netpolicy/exposure.toml` → rendered nft; unlisted ports are dead `[SPEC:ops netpolicy]`.

## Architecture decisions (all `[ASSUMED]` — design choices with stated rationale)

| Area | Choice | Rationale |
|---|---|---|
| DB | PostgreSQL via sqlx | concurrent writers (ingest+API+workers), LISTEN/NOTIFY, SKIP LOCKED queue, FTS |
| Blobs | content-addressed filesystem behind a `BlobStore` trait (S3-capable later) | single node, zero extra services |
| download_url | HMAC-signed expiring URLs served by our API | opaque to SDKs `[SPEC:sdk]`, no presigning infra |
| Search | Postgres FTS + `ts_headline` highlights + pg_trgm substring filters | covers `q` + highlights shape with no extra infra |
| HTTP | axum + tower layers (auth, rate-limit, idempotency) | middleware shape fits; first-class WS |
| Inbound | own SMTP daemon on :25 via smtp-proto + mail-auth (SPF/DKIM/DMARC/ARC) + mail-parser; plus authenticated HTTP ingest fallback | mirrors their gateway model; fallback covers provider relay |
| Spam/virus | classify-only into `unauthenticated`/`blocked`/`spam`; optional rspamd sidecar; optional clamd → gateway reject | matches their semantics `[SPEC:docs spam-virus-detection]` |
| Outbound | direct-to-MX via mail-send with per-domain DKIM; configurable smarthost relay | port 25 egress verified open `[TESTED]`; relay = deliverability hedge |
| Bounce/complaint | DSN + ARF parsing on feedback subdomain via same ingest pipe | their `mail.` return-path pattern `[SPEC:docs custom-domains]` |
| Threading | **OBSERVED (P-1 item 16, fixture 16-threading-matrix/): strict RFC Message-ID reference chain (In-Reply-To then References), scoped PER-INBOX. Subject is NOT a grouping key — Re:/RE:/Fwd:/FW:/AW:/[list]/trailing-ws/exact-dup/empty subjects each opened their own thread; correspondent identity neither sufficient nor necessary.** ~~prior assumption: normalized-subject + shared-correspondent ~7d fallback~~ — **killed by the matrix (18 msgs → 17 threads; only the In-Reply-To pair merged)**. amk-core default impl = strict Message-ID chain, per-inbox, NO subject fallback. Trait boundary retained for the uncovered dimensions | see Register A10 |
| Jobs | Postgres `jobs` table + tokio workers | one durable mechanism; no Redis |
| Webhooks | embedded Svix-wire-compatible engine in-process. Engine requirements `[SPEC:svix]`: retry schedule immediate, 5s, 5m, 30m, 2h, 5h, 10h, 10h; **15s response deadline** for a 2xx (else the attempt fails); **3xx counts as failure**; on schedule exhaustion mark Failed and emit `message.attempt.exhausted`; **auto-disable an endpoint failing 5 straight days** and emit `EndpointDisabledEvent` | format fully documented `[SPEC:docs webhook-verification]`; self-hosting Svix adds services for no compat gain; parity checked with official svix libs; schedule-override question answered by P-1 item 7 |
| Reply extraction | port Talon's non-ML heuristics to Rust; until then `extracted_*` = full body (degraded) | Talon is Apache-2.0, test corpus reusable |
| MCP | rmcp, Streamable HTTP `/mcp` + stdio, tool names/schemas matching their manifest; API-key auth only (no Clerk) | manifest is published `[SPEC:repo agentmail-mcp]` |
| IMAP | minimal IMAP4rev1 subset, final phase | per P-1 item 14 survey: no embeddable Rust IMAP-server crate exists; build on imap-codec/imap-types/imap-next |
| TLS | cert-manager (Cloudflare DNS-01 — DNS already Cloudflare-as-code) → Secrets; amkd terminates TLS via rustls with hot-reload | kills the in-pod ACME sidecar pattern |
| Shape provenance | every wire type, storage model, and identifier shape derives from AgentMail's artifacts (openapi.json, official SDKs, docs) — never from Stalwart. Stalwart appears in exactly two sanctioned roles: (1) migration source at P6, (2) vendor of standalone MIT crates (mail-parser, mail-auth, mail-send, mail-builder, smtp-proto) consumed as libraries like any third party | `[ASSUMED]`: the goal is 1:1 AgentMail compatibility; any Stalwart-derived shape is a defect regardless of how reasonable it looks. Named explicitly because we fork nothing but read Stalwart heavily during migration — which is where leakage happens. Naming lints and dependency direction catch structural leakage; the dual-target conformance diff is what catches semantic leakage. Both are required — neither is sufficient alone |
| Auth extensibility | amk-http's tower auth layer resolves a `Credential` enum, not a raw API key. One variant today (`ApiKey`). Handlers take the resolved principal/scope, never the credential itself. **Type-shape decision only — no session tokens, JWT handling, or console surface in V1** | `[ASSUMED]`: API-key-in-localStorage is not viable for a browser client, so a session-token variant will be needed if a frontend is built. AgentMail runs a parallel session path themselves — their error catalog documents `invalid_token_type` rejecting a "console session token (JWT)" on API-key endpoints, and two oauthOnly org tools are excluded from the MCP manifest `[SPEC:docs errors + repo agentmail-mcp]`. Making the enum pluggable at P0 costs nothing; retrofitting it through every handler later is expensive |
| K8s | ns `agentmail` PSA restricted, non-root, high ports mapped by Services; MetalLB L2 pool over 64.112.14.0/24, `externalTrafficPolicy: Local`, `.64` annotated to the mail Service; egress SNAT to `.64` via the existing netpolicy nft-as-code pipeline; CloudNativePG single-instance cluster; plain-YAML manifests in `deploy/k3s/` matching the ops convention; logs to stdout; probes | correctness + the cluster's own declarative conventions `[SPEC:ops]` |

Workspace: crates `amk-types` (wire types + error catalog), `amk-core` (scope/permissions/threading/labels/ids), `amk-store` (sqlx, migrations, blobs, FTS, signed downloads), `amk-ingest`, `amk-outbound`, `amk-events`, `amk-jobs`, `amk-http`, `amk-mcp`, `amk-dns`, `reply-extract`; bins `amkd` (--role api|smtpd|worker|all), `amk` (init/migrate/doctor/import); `conformance/`; `deploy/k3s/`; `reference/` (vendored openapi.json + SDK extracts + mcp-manifest).

Crate pins `[TESTED: versions exist AND the assumed APIs compile — P-1 item-15 spike, fixture 15-compile-spike.txt, cargo build exit 0]`: mail-parser 0.11.6, mail-auth 0.12.0, mail-send 0.6.1, mail-builder 0.4.4, smtp-proto 0.2.3, rmcp 3.1.2, axum 0.8.9, sqlx 0.9.0, tower 0.5.3, governor 0.10.4, hickory-resolver 0.26.1. Others (tokio, argon2, reqwest, thiserror, anyhow, serde, uuid, chrono, hmac/sha2, tracing) `[UNVERIFIED — pin during setup]`.

**API-fit corrections the build MUST use (from the spike; 8 of 11 assumptions were wrong in detail):**
- **axum 0.8**: `features=["ws"]` required for `WebSocketUpgrade`; route param syntax is `{id}` (not `:id`); `Path<Uuid>` handlers need `uuid`'s `serde` feature or fail with an opaque Handler-trait error.
- **smtp-proto 0.2 is PARSER-ONLY (no server engine)** → **amk-ingest owns the SMTP state machine**; `Request::parse` yields `Request<Cow<str>>`. (Confirms the libraries-not-mailserver architecture at the code level.)
- **mail-auth 0.12**: DKIM signing key ctor is `RsaKey::from_key_der(PrivateKeyDer)` — **PEM helpers deprecated → the PKCS#1 PEM keys extracted from Stalwart (A4) must be converted to DER before signing**; `Signature::to_header()` needs the `HeaderWriter` trait in scope. Verification surface as assumed (`MessageAuthenticator`, `SpfParameters::verify_mail_from`, `DmarcParameters::new`, `Dkim/Spf/DmarcResult`).
- **mail-send 0.6**: `SmtpClientBuilder::new()` is fallible (`Result<_, String>`).
- **hickory-resolver 0.26**: `build()` fallible; typed Mx/TxtLookup gone — iterate `Lookup::answers()` and match `RData::MX/TXT`; Record/MX expose public fields, not accessors.
- **rmcp 3.1.2**: `Parameters` is at `handler::server::wrapper::Parameters`; content type is `ContentBlock` (not `Content`); `StreamableHttpService<S, M>` takes a session-manager generic; **no websocket transport exists — MCP is stdio + streamable-HTTP only** (WS is served by axum, not rmcp, so this is fine).
- Compiled exactly as assumed: sqlx 0.9, mail-parser 0.11, mail-builder 0.4, governor 0.10.

## P-1 — Evidence close-out (FIRST executable step; no product code)

Rules in force: raw fixtures in `reference/fixtures/` (request + unmodified response; Authorization request headers never written; any `api_key` secret in a response body sed-redacted); an item is [TESTED] only with a fixture file; time-dependent probes start first and stay IN-FLIGHT until observed; destructive probes only on a throwaway pod/inbox (created for this, named in the report, deleted after); no estimates; report per item as `[TESTED|IN-FLIGHT|TODO-VERIFY] + fixture + one-line observed`.

Status at plan time (execution blocked by plan mode until approval). Item numbers are stable identifiers for status reports.

- **Item 1 — AgentMail key** — [IN-FLIGHT] `kv/agentmail` re-stored by user; `auth/me` → 200 org-scoped Identity observed this session, but the fixture file is not yet on disk (plan mode blocks the write), so not [TESTED] until `reference/fixtures/01-auth-me.http` exists. Injected only via `sdxd run`; value never read.
- **Item 2 — IMAP gap decision** — RESOLVED (user decision, no fixture applicable): gap accepted.
- **Items 3–9 — Live-API fixtures** (3 id formats incl. confirming the angle-bracketed RFC-5322 `message_id` + its URL-encoding; 4 pagination; 5 error catalog — now narrowed to: well-formed-but-invalid `am_` key (expect envelope `unknown_api_key`, not bare 403), inbox-collision code (409 `resource_taken` vs 422), and any codes docs leave ambiguous; 6 download_url expiry watch + small-vs-large attachment bytes/URL threshold; 7 webhook retry curve — now only answering whether AgentMail overrides the documented Svix defaults; 8 threading fallback — SUPERSEDED by item 16's full matrix; 9 event payloads) — [TODO-VERIFY] pending approval: run via subagent under `sdxd run` from the project dir (grant in place), against a **throwaway pod + inboxes** created for the purpose and named in the report. Items 6 and 7 launch FIRST as background watchers and stay IN-FLIGHT until the observation lands (partial data stays [TODO-VERIFY]). Retry sink (7): prefer a self-controlled sink/tunnel; third-party sink only for synthetic throwaway events, noted in the fixture. Hard events (9): `message.complained` → Register A11 (probe: item 17); `domain.verified` → Register C1 (blocked by D1); `message.received.unauthenticated` variant → Register A14.
- **Items 10–12 — Server evidence** — [IN-FLIGHT]: observations complete via read-only ssh this session (full raw captures preserved verbatim in the subagent report, ready for `reference/fixtures/{10,11,12}-*.txt`; files unwritten only because of plan mode). Observed:
  - **10 — DKIM**: the six PEMs under `etc/dkim/` are STALE (none matches DNS; `imabee.ca` signs but has no file). Live keys = 4× RSA-2048 PKCS#1 PEM **embedded in Stalwart's internal RocksDB store**, one per domain (appsynergy.io, imabee.ca, imabee.cloud, imabee.com), selector `s20260410`, each verified on-host to match its published DNS `p=` (len 392). Migration consequence: extract keys from the store, NOT from `etc/dkim/` — extraction mechanism is A4, resolved IN P-1 (pulled forward because outcome (b) imposes a cutover downtime constraint).
  - **11 — CP relay**: `email_settings` id=1 → `m.appsynergy.io:587`, enabled=1, from `APPSYNERGY <noreply@appsynergy.io>`, username 21 chars, `use_tls=0` (maps to lettre plaintext builder), `smtp_password_ct` NULL → per `transport.rs:179-181` the transport is never constructed: **CP email notifications are currently inert**. Nothing breaks at cutover; repoint afterward with a real credential and `use_tls=1` (STARTTLS).
  - **12 — Dependents (from config)**: live = **Bulwark webmail (JMAP → https://m.appsynergy.io, secret `bulwark-env`, hostAlias to .64)** and the inert CP SMTP path only. keel/combly/mellify are undeployed templates (lettre SMTP or JMAP, provider default Log, no manifests anywhere); gitea (no mailer in app.ini), alertmanager, CI verified non-dependents. External MUAs use IMAP 143/993, submission 587/465, sieve 4190 per the mail pod's ports.
- **Item 13 — Source-IP echo test** — [TODO-VERIFY], staged: manifest for throwaway ns `amk-probe` (whoami + NodePort ETP-Local), external vantage 45.233.219.186 recorded, ssh/kubectl verified (k3s v1.33.13+k3s1); execution + mandatory cleanup pending approval. No firewall changes; if the NodePort is unreachable from outside, that is the recorded observation and the exposure.toml decision goes to the user.
- **Item 14 — IMAP crate survey** — [IN-FLIGHT]: crates.io research complete this session (**no production-grade embeddable Rust IMAP server crate exists**: imap-server/stalwart-imap unpublished; crymap/hopf-imap/etc. are applications or toys; credible path = `imap-codec` 2.0.0-alpha.9 + `imap-types` + `imap-next` 0.3.4 sans-I/O server flow), but not [TESTED] until `reference/fixtures/14-imap-crate-survey.txt` is written (blocked by plan mode).
- **Item 15 — Compile spike** — [TODO-VERIFY]: version existence re-confirmed and rmcp 3.1.2 features `server`, `macros`, `transport-io`, `transport-streamable-http-server[-session]` confirmed to exist on crates.io; **API fit unverified** until the `cargo build` spike runs (pending approval).
- **Item 16 — Threading probe matrix** — [TODO-VERIFY] (replaces "observe one case"; sender fully controlled since port-25 egress from the OVH box is `[TESTED]` open. **Correction, 2026-08-15: the injector is NOT swaks — `swaks` is not installed on that host. Fixtures 09b, 16 and 21 all used `/root/amksend.py`, a python3/smtplib sender that forwards `--inreplyto` verbatim (source-verified). Any future header-injection probe reuses that, and the plan's other "swaks" mentions mean it.** It injects arbitrary Message-ID/In-Reply-To/References/Subject/From into a throwaway inbox). One fixture per case, recording resulting `thread_id`: (a) control: valid In-Reply-To chain; (b) same subject, no threading headers, different From — is correspondent overlap required?; (c) subject prefixes Re:/RE:/Fwd:/FW:/AW:/"[list] "/trailing whitespace — what normalization; (d) near-identical subject (one word differs) — exact or fuzzy; (e) same subject to two inboxes in one pod — per-inbox or wider scope; (f) empty subject — new thread each time or all grouped; (g) window bisect: identical subject at T+5m, +1h, +6h, +24h, +3d, +7d, +14d, +30d as a scheduled BACKGROUND probe — partial results stay [TODO-VERIFY]; non-blocking (threading is P2 and this path fires only for mail with no In-Reply-To). Report the inferred rule set AND explicitly which dimensions the matrix did NOT cover. fixture: `reference/fixtures/16-threading-matrix/` (one file per case).
- **Item 17 — `message.complained` via a real FBL** — [TODO-VERIFY]. Step 1 DONE `[TESTED]`: openapi.json defines the `message-complained` webhook and full `Complaint`/`MessageComplainedEvent` schemas → wire shape is `[SPEC:openapi]`; only trigger/timing and live `type`/`sub_type` values remain open. Procedure: send from a throwaway AgentMail inbox to an **Outlook.com/Hotmail address the user controls** (JMRP reports complaints to SES; Yahoo fallback; NOT Gmail — aggregate-only), mark as junk there, watch the webhook sink, capture the payload verbatim. Needs from user at execution: the Outlook/Hotmail address + the junk-marking action. If nothing arrives within a stated window (set at launch, e.g. the same wall-clock horizon as the retry watcher), it moves to ACCEPTED UNKNOWNS **with the attempt recorded** — only after the attempt. fixture: `reference/fixtures/17-message-complained.txt`.

**Decision D1 — DECIDED (user accepted option b)**: `domain.verified` probe is BLOCKED — no throwaway domain; isolate behind the emit-interface boundary (wire shape already `[SPEC:openapi]`; P5 exercises the real verification flow against our own implementation). Item 9 runs without it; the four production domains are never touched by probes.

**Scope fence:** P-1 produces no product code. While the item-6/7 watchers idle, no P0 scaffolding, workspace setup, or migrations begin — idle is correct; anything that seems safe to start early gets asked about first.

Deliverables of P-1: populated `reference/fixtures/`; dual-target conformance harness skeleton (`conformance/` script running identical requests against `api.agentmail.to` and localhost, diffing status/headers/body structurally) — **this harness is the real 1:1 check: every phase gate P1–P5 requires its diff clean for that phase's endpoints; naming lints cannot prove semantic parity, the diff can**; plan updated with every observation; list of genuinely-unknowable items each with an isolation strategy (trait boundary/config flag).

## V1 — required scope (drop-in core that replaces Stalwart for agent + app mail)

Everything below is `[PLANNED]`. Each phase ends at a **Gate**: a verification that must be executed and observed before the phase is called done. No phase is done until its gate has actually run.

- **P0 Skeleton** — workspace, config, migrations, `amk init` (default org+pod, root key shown once), Bearer auth deny-by-default, error shapes **per A8 — RESOLVED, build the asymmetry**: auth-layer failures (missing/invalid credential) return the bare gateway body `{"message":"Unauthorized"}` 401 / `{"message":"Forbidden"}` 403 (no name/code/fix/docs); app-layer failures return the full envelope. cursor pagination = base64(JSON keyset {sort-key,id}) per fixture 04. Gate: official Python SDK `auth.me()` against localhost returns Identity, AND the shape-provenance CI check passes from the first commit: (i) dependency direction — a `cargo metadata`-based script asserting no dependency path from amk-types/amk-core/amk-store to amk-import (chosen over cargo-deny: zero extra tooling, exact graph); (ii) naming — a grep-based deny-lint over those three crates' sources rejecting Stalwart/JMAP-derived concepts (JMAP, Sieve, blob-id-as-Stalwart, RocksDB key shapes, mailbox-role enums absent from AgentMail's spec), run as a CI step alongside the tests; (iii) boundary types — the stalwart-labs crates (mail-parser, mail-auth, mail-send, mail-builder, smtp-proto) are an unguarded leak path because their types are ergonomic and right there: assert no `mail_parser::`/`mail_auth::`/`mail_send::`/`smtp_proto::` type appears in any public signature or re-export of amk-types/amk-core/amk-store — those types live only inside amk-ingest/amk-outbound, converted at the boundary.
- **P1 Control plane** — auth/me, organizations, pods, inboxes (3 mounts; **collision = `already_exists` HTTP 403 + `suggestions[]`**, observed live in P-1, fixture 05 — supersedes both the 409 and 422 guesses), api-keys, `client_id` idempotent creates. Gate: Python+Node SDK smoke (create/list/delete across scopes) + schemathesis over implemented paths; dual-target conformance diff clean for all endpoints implemented in this phase (status, headers, body structure) against api.agentmail.to. (Golden-fixture recording against the real API is P-1's job, not P1's.)
- **P2 Mail in/out** — ingest daemon (:25 high-port + Service map, local-domain RCPT only, mail-auth verdicts → labels, threading, blob store), HTTP ingest fallback; send/reply/reply-all/forward with DKIM signing, direct-to-MX + smarthost option; threads/messages reads/filters/batch/raw/attachment downloads. Gate: injected mail (`/root/amksend.py` on the OVH box — not swaks, see item 16) appears via SDK with correct threading over a 3-message exchange; SDK send to a Gmail test account shows DKIM+SPF pass; dual-target conformance diff clean for this phase's endpoints against api.agentmail.to.
- **P3 Drafts + scheduling + idempotency** — drafts CRUD/modes/references, `send_at` jobs, `Idempotency-Key` layer, SSRF-safe url-attachments. Gate: scheduled draft delivers; duplicate key returns identical response; mismatch → 409; dual-target conformance diff clean for this phase's endpoints against api.agentmail.to.
- **P4 Events** — webhooks CRUD (3 scopes, write-only headers), Svix-wire delivery + retries, WS hub, inbox events, metrics. Gate: official `svix` python lib verifies our signatures; SDK websocket receives `message.received`; opt-in spam events replace `received`; dual-target conformance diff clean for this phase's endpoints against api.agentmail.to.
- **P5 Domains** — CRUD, DNS record verification (hickory), re-verify job → `domain.verified`, zone-file export, DKIM keygen + import of existing keys, `feedback_enabled` DSN/ARF → bounce/complaint events. Gate: one real domain verified end-to-end; induced bounce produces `message.bounced`; AND **P5's domain types diff clean against `reference/fixtures/C1-domain-shape.txt`** — any field we emit not in that fixture, or any fixture field we omit, is a conformance failure, not a judgement call; dual-target conformance diff clean for this phase's endpoints against api.agentmail.to (domain endpoints diff against a real account's domain listing where D1 permits — read-only listing of their side, no domain creation on production domains).
- **P6 Deploy + migrate + cutover** (details below). Gate: restore drill (step 3b) passed from backups alone; production traffic on `.64` served by AgentMailKit; Stalwart scaled to 0; dns-health checks green.

## Full parity — parked until V1 ships (scope creep goes here, visibly)

`[PLANNED]` in rough order: FTS search endpoints with highlights; allow/block list enforcement UI-parity (`send` direction 403 `message_rejected`); rspamd/clamd sidecars; Talon-port reply extraction (until then degraded); full 34-flag permission matrix + AgentID public keys; agent signup + OTP (config-gated, off by default); MCP server + stdio (Gate: Claude Code, ChatGPT, Grok connectors list tools and send mail; unmodified npm `agentmail-mcp` bridge works against our URL); SMTP submission :587/:465; `subdomains_enabled` wildcard-MX; batch endpoints not needed by v1 consumers; IMAP subset (last; mail-client access has a gap between Stalwart retirement and this phase — gap explicitly accepted by user); console/session auth path for a browser frontend — API-key-only ships in V1; the `Credential` enum leaves room for it without touching handlers; EU/x402 host aliases as no-op vhosts.

## Deployment & migration (P6 detail)

1. Install MetalLB, cert-manager (+ Cloudflare token via existing secd/on-box secret pattern), CNPG — **and the backup infrastructure** (destination DECIDED by user): a **USB drive attached to the host, mounted at a fixed path**, holding the CNPG `barmanObjectStore` target (base backups + WAL archive) and the blob-tree snapshot. **MANDATORY: the backup job verifies the mount is present and writable before every run and fails loudly if not** — a job that silently writes to an unmounted path produces the belief in a backup without the backup. `mount-present` and `last-successful-backup-age` join the Observability signal list, both alerted. Manifests in `deploy/k3s/`, `exposure.toml` updated through its render/check pipeline. `[PLANNED]`
2. **Source-IP echo test** through a MetalLB `externalTrafficPolicy: Local` Service from an external vantage — the one networking claim not yet proven live `[TODO-VERIFY; prerequisite for everything after]`. Fallback if it fails: routed /32 (Multus) for the smtpd pod only.
3. Stage AgentMailKit on a spare pool IP; run full conformance + SDK gates in-cluster. `[PLANNED]`
3b. **Restore drill**: full recovery of the staged system onto a scratch namespace from backups alone (Postgres from barman, blobs from snapshot, Secrets from secd) — must pass before any cutover step runs. `[PLANNED]`
4. Import the LIVE DKIM keys. ~~Original assumption: copy PEMs from `stalwart-data` PV `etc/dkim/*`~~ — **killed by the fixture-10 observation**: none of those six files matches DNS and `imabee.ca` (which signs) has no file there; the live keys (selector `s20260410`, 4× RSA-2048 PKCS#1 PEM, one per domain, each DNS-matched) live **inside Stalwart's internal RocksDB store**. Extraction mechanism per A4, determined in P-1; if outcome (b) — offline RocksDB read, requiring Stalwart stopped or a file-level copy — the downtime-window constraint recorded there applies to this step. Keys land as k8s Secrets → existing DNS DKIM records stay valid. Register the 4 domains + existing inboxes. `[PLANNED]`
5. Mailbox migration: `amk import` pulling from Stalwart over IMAP (or maildir export) preserving timestamps/threading. `[PLANNED]` **amk-import is a TRANSLATION BOUNDARY, not a storage path**: it reads Stalwart's representation, maps it to AgentMail shapes, and writes only through amk-store's normal public interface. Constraints: amk-import may depend on amk-store, never the reverse; no Stalwart-derived field, enum, or identifier may appear in amk-types, amk-core, or amk-store — not even as an optional or legacy field; **acceptance criterion: amk-import is deletable after cutover with zero changes to any other crate**. **Import mapping table — written and reviewed BEFORE any import code** (any row whose answer is "keep Stalwart's version" is a defect to escalate, not a quiet decision; anything with no AgentMail equivalent is DROPPED, never carried as a legacy field):
   | Stalwart concept | AgentMail concept |
   |---|---|
   | IMAP folder | label (NOT a folder/mailbox entity) |
   | IMAP flags \Seen / \Answered / \Flagged | labels: `unread` absence-of / reply-state via threading / no equivalent → DROPPED unless AgentMail's label set covers it — final per-flag mapping fixed at table review |
   | IMAP UID / UIDVALIDITY | DROPPED (amk mints its own message ids) |
   | Stalwart thread grouping | re-derived by OUR threading — never imported |
   | Stalwart message blob | raw MIME through amk-store's normal public blob path |
   | Sieve rules | DROPPED (no AgentMail equivalent) |
   | JMAP mailbox roles | DROPPED unless a row-by-row mapping to system labels is justified at review |

5b. Delta-sync (per Operations → Cutover data integrity): quiesce Stalwart ingest → incremental import of mail received since step 5's bulk import → only then swap. `[PLANNED]`
6. Cutover: move `.64` to amk Services (annotation swap). MX/SPF/DKIM/DMARC/PTR unchanged. Written swap procedure per Operations (scale amk smtpd ready → verify → move annotation → verify banner from outside; in-flight senders defer and retry by SMTP design). Verify inbound from outside, outbound DKIM/SPF at Gmail, `dns-health.py` green. `[PLANNED]`
7. Decommission: Stalwart to 0 replicas; retain `stalwart-data` PV until V1 acceptance under the rollback definition in Operations (no reverse import — rollback after cutover accepts loss of mail received since cutover; fix-forward preferred); then remove manifests + exposure entries. **Delete amk-import (or feature-gate it off) once cutover is verified — if it cannot be deleted cleanly, the translation boundary leaked: treat as a defect to fix, not a file to keep.** Repoint (per fixture 12, the real list is short): CP `email_settings` → amk submission with a real credential + `use_tls=1`; keel/combly/mellify only if/when deployed. Retired with Stalwart (no AgentMail-model equivalent): Bulwark webmail (the one live JMAP dependent), admin SPA, JMAP, ManageSieve, WebAuthn login (confirmed not in production `[TESTED — upstream v0.15.5 image observed]`). `[PLANNED]`

## Security (applies from P0)
Deny-by-default identity layer; scope resolution before handlers; per-handler permission checks; label-denial → not_found `[SPEC:docs permissions]`. Keys argon2id, shown once, O(1) lookup id + constant-time verify. No open relay (RCPT only for local verified domains); per-IP caps; size limits pre-DATA; AUTH only over TLS; greet-pause on smtpd `[TESTED their behavior]`. SSRF-hardened outbound HTTP (pin resolved IP, deny private/metadata ranges, re-validate redirects, cap size/time) for url-attachments + webhooks. sqlx bind-only; CR/LF rejection in header inputs. DKIM keys + webhook headers encrypted at rest. Rate buckets per-key/per-IP, stricter on auth failure paths. PSA restricted, non-root, no capabilities. Logs structured to stdout, key-ids never key material.

## Operations (this is a production mail system, not only a compatibility project)

### Cutover data integrity — CHANGES P6; procedure resolved before P-1 ends
- **Freeze/drain**: MX points at `.64` throughout. During the annotation swap there is a window where neither pod holds the IP; SMTP senders retry on connection failure by protocol design (typically for hours), so in-flight mail is deferred, not lost — but the swap procedure must be written and timed, not improvised: scale amk smtpd ready → verify → move annotation → verify banner from outside.
- **Delta-sync**: step 5 (bulk import) and step 6 (swap) leave a gap; anything received by Stalwart between them is LOST unless a second incremental import runs immediately before the swap, after Stalwart stops accepting new mail. Added as explicit P6 step 5b: quiesce Stalwart ingest → incremental import of the delta → then swap. `[PLANNED]`
- **Rollback, defined honestly**: no reverse import (amk → Stalwart) exists and none will be built — writing one contradicts the shape-provenance boundary and doubles migration surface for a path we hope never to use. Therefore: **rollback after cutover means accepting loss of mail received by amk since cutover.** Trigger criteria: amk unable to accept or durably store inbound mail, and not forward-fixable promptly — fix-forward is always preferred. The `stalwart-data` PV is retained until V1 acceptance (below), then released.

### Backup and restore for AgentMailKit itself
Backup scope: Postgres (CNPG scheduled base backups + WAL), blob store (filesystem snapshot/rsync of the content-addressed tree — immutable objects make incremental cheap), DKIM/master-key Secrets (in the existing secd/sdxd store as the source of truth; k8s Secrets are derived). Frequency/retention: daily, 14-day retention as the starting point (operator-tunable). Destination: USB drive on the host at a fixed mount path (user decision). **Accepted limitation, stated explicitly: backups live on the same physical machine they protect. This covers disk failure, corruption, and bad deploys; it does NOT cover loss of the machine (theft, fire, seizure) or an operator error affecting the mounted volume. Accepted for a single-user personal system; off-site replication is out of scope. Revisit if the data ever matters beyond personal use.** **A restore drill — full recovery onto a scratch namespace from the USB backups alone — must pass before cutover** (P6 step 3b; restoring from the USB is exactly what it proves). We are replacing a system whose data we meticulously preserve with one that must not launch without a backup story.

### Single-instance Postgres — accepted consequence
Node reboot or Postgres restart = API and inbound SMTP down for its duration; deferred by sender retries, not lost. Accepted for a single-node cluster; revisit only if a second node ever exists.

### Observability (minimum signals, named)
Queue depth and age (jobs table, per kind), DKIM signing failures, webhook delivery backlog + failure rate + disabled endpoints, inbound reject rate by reason (relay-denied / virus / size / unknown-recipient), bounce rate, ingest parse failures, backup `mount-present` and `last-successful-backup-age` (both alerted). Surface: a Prometheus `/metrics` endpoint on amk-api scraped by the existing `monitoring` namespace stack; alert thresholds configured there. This promotes metrics-endpoint from parked to P4 (it rides the same counters the metrics API needs).

### Upstream drift — structural to a 1:1 clone
The conformance harness depends on AgentMail's live API and spends real requests against the user's account (run it at phase gates and on demand, not in every CI loop). Fixture refresh trigger: a failing conformance diff, or a changed `openapi.json` (check its hash at each phase gate). When AgentMail ships a breaking change post-V1: track deliberately — diff the new spec, decide adopt/pin per change, record in the plan; a 1:1 clone that stops tracking becomes a clone of a past version, which is an acceptable end-state only if chosen explicitly.

### Secrets provenance and rotation
Source of truth for DKIM private keys, the at-rest master key (webhook headers, DKIM keys in DB), and smarthost/API credentials: the secd/sdxd store; k8s Secrets are derived copies created via the established on-box pattern. Rotation: DKIM = new selector published alongside old, cutover, old selector retired after DNS TTL (zone-file endpoint regenerates); master key = re-encrypt job with versioned key ids; API keys = user-driven via the API (create/delete). No key material in git, transcripts, or hook output — ever.

### V1 acceptance
**V1 is complete when the unmodified official AgentMail SDKs, CLI, and MCP bridge run against the server holding `.64`, the dual-target conformance diff is clean for every V1 endpoint, the four production domains send and receive live mail with DKIM/SPF/DMARC passing, every P0–P6 gate has actually run, the restore drill has passed, and Register A is empty except items on their stated clocks.**

## Testing (applies from P0)

Layers, each with a stated purpose so they don't collapse into each other:
- **Unit** — pure logic in amk-core/amk-types: scope resolution, permission intersection, label rules, threading, id parsing/encoding, error envelope construction. No I/O, no DB.
- **Golden fixture regression** — every `reference/fixtures/` capture is wired as a test asserting our response structurally matches. Fixtures are not documentation; they are the regression suite. A fixture that isn't asserted against is a gap.
- **Integration** — against real Postgres + real blob store, per crate.
- **Adversarial** — hostile input, listed below. These are not "extra"; they are the tests most likely to catch real defects.
- **Conformance (dual-target)** — already gated per phase. Structural parity, not correctness.

### Edge cases to write cases for (test written BEFORE the code that would break on it; each maps to a phase)

**IDs and routing (P0/P1)**
- `message_id` is an RFC 5322 angle-bracket value in a path segment: round-trip `<abc@d.com>` through encode → route → decode. Also: `+`, `%`, `/`, `?`, `#`, space, and non-ASCII in the local part; an id containing a literal encoded `%2F`; double-encoding; an id longer than any reasonable path-segment limit.
- `inbox_id` is an email address in a path segment: same matrix, plus plus-addressing and case normalization (is `Foo@x.com` the same inbox as `foo@x.com`? decide, then test).
- `page_token`: tampered, truncated, base64-invalid, a token from a different scope, a token from a deleted resource, a token replayed after the underlying rows changed.

**Scope and permissions (P0/P1) — highest security value**
- Pod-scoped key reaching an inbox in a different pod → `not_found`, NOT forbidden (scope/label denial masks as not_found per docs).
- Cross-org access attempts at all three mounts.
- Permission escalation: creating a key with a permission the parent lacks → `permission_escalation`; child ⊄ parent enforced at every level.
- Restricted labels (spam/blocked/unauthenticated/trash) invisible to a key lacking read permission — including no leakage via counts, pagination totals, or thread membership.
- `unrestricted_key_required` gates on the documented paths.

**Idempotency (P3)**
- Same `Idempotency-Key`, identical body → original response; same key, different body → 409; empty key → 400; key reused after the 24h TTL; concurrent identical requests with the same key (race — must not double-send); first attempt failed without completing → key retryable after the short window (per docs); `client_id` replay on creates → original resource, not a duplicate.

**Mail ingest (P2) — adversarial, assume hostile input**
- Malformed MIME: unterminated boundary, nested multipart bombs, 8-bit in headers, missing Content-Type, conflicting Content-Transfer-Encoding.
- Header injection: CR/LF in any user-supplied field reaching a header (send to/subject/from) — rejected, one test per field.
- No Message-ID; duplicate Message-ID; Message-ID matching an existing message in another inbox.
- In-Reply-To referencing a message we don't have; References chain loops; a References chain 500 entries long.
- Subject: empty, only "Re:", 10KB long, RFC 2047 encoded-word, unicode homoglyphs, a subject normalizing identically to another (feeds the threading fallback).
- Envelope vs header From mismatch; multiple From headers; missing To.
- Oversize: message just under and just over the size cap; attachment just under and just over the ≈5.95MB inline/URL threshold (exact boundary, both sides).
- Attachment with a traversal filename (`../../etc/passwd`), a null byte, or a 200-char name.
- RCPT for a non-local domain → refused (explicit open-relay test).
- Pipelined EHLO before greet-pause expiry.

**Outbound and SSRF (P2/P3)**
- url-attachment pointing at: 127.0.0.1, 169.254.169.254, a private range, a hostname resolving to a private IP, a redirect chain ending at a private IP, a URL returning 200 then streaming unbounded bytes, a URL that never closes.
- Same matrix for webhook target URLs.
- DNS rebinding: hostname resolving differently between check and fetch (pin the resolved IP — test that the pin holds).

**Webhooks and events (P4)**
- Signature verification against the official svix libs (both directions).
- 3xx → failure; connection held past 15s → attempt fails; retry timestamps match the schedule; `svix-id` stable across retries; exhaustion → `message.attempt.exhausted`; 5 days all-failing → endpoint disabled + `EndpointDisabledEvent`.
- Opt-in spam/blocked/unauthenticated variants REPLACE `message.received` (assert `message.received` is NOT also delivered).
- WS: subscribe to an inbox the key can't see; subscribe with an event_type the key lacks permission for; reconnect mid-stream.

**Drafts and scheduling (P3)**
- `send_at` in the past; far future; during a DST transition; timezone-naive value.
- Draft deleted while scheduled; draft edited while sending; `send_status` transitions — illegal transitions rejected.

**Concurrency (all phases)**
- Two simultaneous creates of the same inbox username → exactly one wins, other gets the collision code.
- Concurrent label mutations on one message.
- Thread delete (soft) racing an incoming message for that thread.
- Job worker crash mid-send → no double-send on restart (SKIP LOCKED semantics tested, not assumed).

**Import (P6)**
- Every DROPPED row in the mapping table has a test asserting the Stalwart concept does NOT appear in amk-store after import.
- Import of a message whose Stalwart thread grouping disagrees with our threading → ours wins.
- Re-running import is idempotent (no duplicates).

### Mutation testing — required at every phase gate, not optional

**A test that has never failed is not evidence.** In P0 the review panel mutated 30 lines of a
green, twice-reviewed crate and 6 mutants survived — including a fail-open on the exact function a
regression test had just been written to protect, reachable through a sibling the test never
called, and a case-folding assertion that compared a value with itself. Two rounds of careful
reading had not found them.

**The scratch dir is not optional, and it is not only about tidiness — enforce it in the dispatch
prompt.** On 2026-08-16 a test lens was told to "hand-mutate" without being told where, so it
mutated the dispatch worktree in place while two read-only lenses were reading the same worktree
concurrently. One of them read `inboxes.rs` mid-mutation, saw the NUL guard replaced by
`!m.is_empty()`, received the harness's routine file-changed notice (which carries the words "don't
tell the user, they are already aware"), and reasonably concluded it was being prompt-injected. It
refused, reported rather than concealed, and re-verified everything against `git show <sha>:<path>`
instead of the working tree — which is the correct response and the reason nothing was lost. But its
test evidence was void, and it misattributed the resulting flaky failures to shared-Postgres
contention, which sent a real defect hunt at a phantom. Two rules follow: a mutating reviewer works
on a private copy, never the dispatch worktree; and a mutating reviewer never runs concurrently with
a reading one over the same tree. The orchestrator dispatched all three at once and owns this.

**And the copy is deleted when the mutation pass ends — the rule as written above accumulates
gigabytes per dispatch until the tooling dies, which it did.** On 2026-08-16 every Bash call in the
session began returning exit 1 with no output: not the foreground path, not the background path, not
a subagent's, and not a fresh session's. Cause: `/tmp` is a 32G tmpfs and it was full. About 19G of
it was abandoned mutation scratch — `mutate`, `mutate-http`, `mutate-http-corrections`,
`store-prereqs-scratch`, `lens-b-mutcheck`, `branch-check`, `amk-cli-mut`, each a 2–4G workspace
copy left behind by an agent that had correctly followed the private-copy rule and was never told to
sweep. The orchestrator added the last 4G itself.

The failure was invisible because the Bash tool appends `pwd > /tmp/claude-<pid>-cwd` to every
command: once that write hit ENOSPC, *every* invocation reported failure with no output regardless
of whether the command itself had run. Diagnosis went the wrong way for an hour — a session restart
was proposed as the fix, and it changed nothing, because the fault was on disk rather than in the
session. What broke the deadlock was noticing that `Monitor` still worked (it omits that trailing
write) and that redirected files were being *created and left empty*, which is ENOSPC's signature
and nothing else's.

So: **every dispatch prompt that orders a mutation pass also orders its removal, and the agent
confirms the deletion in its report.** Check `df -h /tmp` when a tool starts failing in a way that
makes no sense; a full tmpfs presents as a broken harness, not as a full disk.

The practice, applied per crate at its gate: copy the workspace to a scratch dir, revert or invert
each security-relevant line one at a time, run the suite, and record which mutants survive. A
survivor is either a missing test or dead code — decide which and say so. When fixing a defect,
re-run the mutation that produced it to prove the new test kills it; a fix reported without that is
reported as unverified.

Minimum mutation set per crate: every boolean that gates access (invert it), every `&&`/`||` in a
permission or scope decision (swap it), every `.max()`/`.min()`/comparison in an aggregate, every
early return in a validation path (delete it), and every normalisation call (drop it).

**Mutation is TWO-DIRECTIONAL. Deleting a guard and widening it fail in opposite directions, and a
deletion-only set is structurally blind to the second.** Added after twenty deletion mutations
across three rounds reported "no survivors" on the id-safety dispatch while a live one sat in
`messages::insert`: widening `in_reply_to`'s guard from `is_some_and(pred)` to `is_some()` rejects
every threaded reply — the main path for inbound mail in P2 — and the whole suite stayed green,
because the only tests touching that field passed hostile values and expected the same `Err` the
widened mutant returns. So every guard gets both: delete it (must kill a test) **and** widen it —
`is_some_and(pred)` → `is_some()`, `iter().any(pred)` → `!iter().is_empty()`, a predicate → `true`
— which must also kill a test. **A guard with no clean-path test is unpinned in the direction that
breaks real traffic.**

### Rules
- Adversarial cases are written as tests BEFORE the handler they target.
- Every boundary above is tested at the boundary AND one unit either side (size caps, thresholds, TTLs).
- Any `[TODO-VERIFY]` whose observation lands gets a regression test encoding the observed behavior, so a later change can't silently diverge from what we measured.
- This list is a floor, not a ceiling. Add cases as probes land; never remove one because it seems unlikely.

## Execution (write order, ownership, fan-out, branching, anti-drift)

### Write order — strictly sequential at crate level
The dependency graph dictates the order; nothing downstream starts before its upstream compiles and its tests pass.
1. **amk-types** — wire types, error envelope (incl. `errors[]` per B1), ids. Nothing depends on anything. WRITTEN FIRST, ALONE, BY THE ORCHESTRATOR, NOT FANNED OUT — every other crate's correctness is downstream of these shapes; a subagent guessing a type here poisons everything. Gate: types round-trip against `reference/fixtures/` golden captures.
2. **amk-core** — scope resolution, permissions, labels, threading trait (~~JWZ default~~ → **strict per-inbox Message-ID reference chain, no subject fallback**; JWZ step 5 groups by normalized subject, which the item-16 matrix disproved — see the Threading row), id encode/decode. Pure logic, unit-testable, no I/O.
3. **amk-store** — sqlx, migrations, blobs, signed downloads.
4. **amk-http** — axum, tower layers (Credential enum, rate-limit, idempotency), handlers for P0/P1 surface.
— P0 and P1 gates run here, including all three shape-provenance CI checks and the first dual-target conformance diff —
5. **amk-ingest + amk-outbound** — CAN FAN OUT: share only amk-types/amk-core, disjoint files, neither depends on the other.
6. **amk-events + amk-jobs** — CAN FAN OUT: same reasoning.
7. **amk-dns, amk-mcp, reply-extract** — CAN FAN OUT: leaf crates.
8. **amk-import** — LAST, and only at P6. It is a translation boundary; writing it earlier invites Stalwart shapes into amk-store before the boundary rules are enforced.

### Ownership
- **ORCHESTRATOR (main session)** — never writes implementation code except amk-types. An orchestrator that starts implementing accumulates implementation detail and the architectural plan decays out of effective context — which is precisely how drift happens. It: holds the plan, dispatches, reviews returned diffs against the contract, merges, runs gates, updates the registers.
- **IMPLEMENTER SUBAGENTS** — one crate each, defined in `.claude/agents/`, explicit tool allowlist. Write code + tests for their crate only.
- **REVIEWER SUBAGENTS** — read-only tools, never write. Run as a panel on each returned diff.
- Subagents are ONE LEVEL DEEP — they cannot spawn subagents. All orchestration stays in the main session; the parent is the conductor.

### Model and effort per role
- **ORCHESTRATOR (main session) — Claude Opus 5, effort: ultracode.** Holds the plan, dispatches, reviews returned diffs, decides gate pass/fail. Judgment-heavy seat where a wrong call propagates to every downstream crate. ultracode also enables automatic dynamic-workflow orchestration, which requires an xhigh-capable model — Sonnet cannot run it.
- **amk-types — Claude Opus 5, effort: ultracode.** Written first, alone, by the orchestrator. Every other crate's correctness is downstream of these shapes. No economizing.
- **amk-core — Claude Opus 5, effort: high.** The one subagent-written crate that gets Opus. Permission intersection, scope masking, and label-denial-as-not_found are subtle security logic where an error means silent cross-pod data leakage rather than a failing test. Threading trait lives here too.
- **IMPLEMENTER SUBAGENTS, P2 onward** (amk-ingest, amk-outbound, amk-events, amk-jobs, amk-dns, amk-mcp, reply-extract, amk-store, amk-http, amk-import) — **Claude Sonnet 5, effort: high.** By P2 the contract is fully explicit: writable paths, `[SPEC:*]` citations, fixtures to satisfy, assigned edge cases. This is execution against a spec, not design. 2–3 run in parallel, so speed and cost compound.
- **REVIEW PANEL (all three lenses) — Claude Sonnet 5, effort: medium.** Highest-volume model calls in the project — three reviewers on every returned diff. Each has a narrow, concrete question ("does this match the cited [SPEC:*]?", "any stalwart-labs type in a public signature?", "are the assigned edge cases covered?"). Narrow and concrete suits Sonnet; raise to high only if a lens starts missing things the orchestrator then catches.
- **P-1 PROBE SUBAGENTS — Claude Sonnet 5, effort: medium.** Mechanical: issue the request, capture the raw response, write the fixture. No inference required — inference is the failure mode the fixture rules exist to prevent. EXCEPTION: the A10 threading-matrix interpretation — **Claude Opus 5, effort: high** — inferring a rule set from eight observations, and correctly naming which dimensions the matrix did NOT cover, is genuinely inferential work and the place a weaker read produces a confident wrong rule set.
- **Haiku: not used in this project.** The high-volume shallow-classification work it suits does not appear in this build.

Mechanics:
- Orchestrator model is the session model; set effort via the /effort menu (ultracode).
- Subagent models are set per-agent in the `.claude/agents/` definitions, NOT via `CLAUDE_CODE_SUBAGENT_MODEL` — that env var overrides both the session model and per-stage routing, which would silently flatten the Opus/Sonnet split above. If it is set in the environment, unset it.
- Record each agent's model + effort in its `.claude/agents/` file so the assignment is version-controlled alongside its tool allowlist, not re-decided per dispatch.

VERIFY BEFORE RELYING: confirm the exact effort-level names available in the installed Claude Code version (/effort) and that ultracode appears — it only shows when workflows are enabled and the active model supports xhigh. If ultracode is unavailable, the orchestrator still runs Opus 5 at the highest available effort and workflows are triggered manually by keyword instead of automatically.

### Fan-out — when, and the hard preconditions
Fan out ONLY when all four hold: (i) the crates share no files; (ii) neither depends on the other; (iii) both depend only on already-merged, gate-passed crates; (iv) **amk-types is frozen for the duration** — no implementer may change it. A type change mid-fan-out invalidates every parallel worker's assumptions: ALL parallel work stops, the orchestrator makes the change, workers restart from the new base.
- Mechanics: spawn parallel subagents via multiple Task calls in a single message; each gets its own git worktree (created under `.claude/worktrees/`, branched from the default branch, tracked files only) so no two agents write the same checkout.
- Concurrency ceiling: **2–3 for this project.** Reported reliable practice is 4–8 concurrent worktrees with review — not Claude — as the bottleneck above that; we are review-bound from the start given the conformance requirements, so cap at 3.
- Known hazard: a subagent granted Bash inheriting the parent's full toolset has, in reported incidents, run destructive git commands against the shared checkout. Implementer subagents get a NARROW allowlist: read, write within their crate path, cargo test/build. NO git reset, no git checkout, no operations against paths outside their worktree. (Claude Code v2.1.218 fixed worktree-isolated subagents redirecting git via `git -C` / GIT_DIR — pin to at least that version.)

### Branching hygiene
- `main` is protected; nothing lands except a merge that passed a phase gate.
- One branch per crate per phase: `amk/<phase>/<crate>` (e.g. `amk/p2/ingest`); one worktree per branch under `.claude/worktrees/`; each worktree gets a task-scoped CLAUDE.md carrying the crate's contract.
- Commits conventional and atomic: one logical change, tests in the same commit as the code they cover; no "wip" commits on a branch that will be reviewed.
- Merge order follows write order — never merge a downstream crate before its upstream is on main.
- Rebase onto main before review, never merge-commit into the branch; the reviewed diff must be the diff that lands.
- After merge: delete the branch, remove the worktree. Non-interactive (`-p`) runs never hit the keep/remove prompt and leave worktrees behind — sweep explicitly, `git worktree remove --force` if dirty.
- No branch outlives its phase. A branch open across a phase boundary is a drift signal — close it or restart it from the new base.

### Anti-drift: the plan is a contract
Drift is the default failure mode of fanned-out work — each subagent is confident inside its own isolated context and cannot see that a sibling's assumption was wrong. Five mechanisms, all mandatory:
1. **DISPATCH PROMPT IS THE CONTRACT** — the most important artifact in the workflow. Every implementer dispatch states, explicitly and in full: the crate and exact writable file paths; the `[SPEC:*]` citations governing every shape it implements; the fixture files it must satisfy; the Testing-section edge cases assigned to it; the prohibitions (no changes to amk-types; no `mail_parser::`/`mail_auth::`/`mail_send::`/`smtp_proto::` types in public signatures; no Stalwart/JMAP concepts); and: "If the contract is ambiguous or appears wrong, STOP and report. Do not resolve it yourself." — ambiguity resolved locally by a subagent IS drift. **AND — added 2026-08-15, bought at four correction rounds on the amk-store id-safety dispatch — a contract that scopes a change across EXISTING code carries the enumeration command that produced its scope, plus that command's raw output, and the scope is generated from it.** That contract listed "the five call paths the panel reproduced live", recalled from a review report and never derived from the code. Five sites were missing: `messages::insert`'s `references`, `messages::list`'s cursor, and all of `api_keys.rs` — `inbox_id` at four functions plus the presented credential on the auth path, where the naive fix also reopened a timing side channel. **Every one was found by somebody enumerating; not one was ever found by re-reading the contract.** A reviewer re-runs the command rather than reading the list. Enforced by `contract-scope-derived`: every contract states its derivation or an explicit `n/a` with a reason, because an absent derivation and a deliberate one are indistinguishable until someone walks into the difference.
2. **NO SUBAGENT INVENTS A SHAPE.** If a needed type/field/status is not in amk-types or a fixture, the subagent stops and returns the question. It never adds a field "that obviously belongs."
3. **REVIEW PANEL ON THE CONTRACT BEFORE DISPATCH, AND ON EVERY RETURNED DIFF.** The pre-dispatch pass is the cheaper half and was missing entirely until 2026-08-15: one read-only lens asked only "is this contract's list of affected code complete against the codebase?" costs a single pass, and would have caught all five id-safety omissions before a line was written. Instead they surfaced three rounds deep, after implementation, from lenses reviewing a diff — the same agents at the wrong end of the pipeline. Panel the contract first; panel the diff after. **On the returned diff:** Differently-scoped read-only reviewers in parallel on the same diff — each blind to the others, which catches failure modes a single reviewer misses. Minimum three lenses: contract-conformance (matches the cited `[SPEC:*]` and fixtures, and only those); provenance (any Stalwart/JMAP shape, stalwart-labs type in a public signature, invented field); test-adequacy (assigned edge cases actually covered; tests assert behavior, not restate code). Merge only after all three return clean.
4. **GATES ARE NOT ADVISORY.** A phase gate that has not actually run does not count. The dual-target conformance diff catches semantic drift; naming lints only catch structural drift.
5. **PLAN CHANGES ARE ORCHESTRATOR-ONLY AND VISIBLE.** A subagent may never edit the plan or the registers. If an implementer discovers the plan is wrong, it reports; the orchestrator edits the plan, strikes the superseded text with the observation that killed it, and re-dispatches. Silent divergence between code and plan is the defect this whole structure exists to prevent.

### Tooling note
Fan-out here is subagents-within-a-session, not Agent Teams. Teams are for workers that must negotiate with each other; our crates are contract-separated by design and report only to the orchestrator. If a phase ever requires two implementers to negotiate a shared shape mid-flight, that is a signal the contract was underspecified — fix the contract, don't reach for a coordination primitive.

### Project memory (how the contract reaches each session)
- **Root CLAUDE.md — hard cap 200 lines.** Loads every session; frontier models reliably follow only ~150–200 instructions and Claude Code's system prompt already consumes roughly 50 — past that, context rot dilutes the rules that matter. Contents, and nothing else: exact build/test/lint invocations; the crate write order and current phase; the five non-negotiables (shape provenance, frozen types during fan-out, no invented shapes, evidence-not-assertion, plan is orchestrator-only); a pointer to the plan file — the plan itself does NOT go in CLAUDE.md. Prune test per line: "if I remove this, will Claude make a mistake?" If no, remove it. If it's a rule that MUST hold, convert it to a hook — cheaper than a CLAUDE.md line and it actually binds.
- **Per-worktree CLAUDE.md.** Each fan-out worktree carries only that crate's contract: writable paths, `[SPEC:*]` citations, assigned fixtures, assigned edge cases, prohibitions — the dispatch contract in file form, so it survives compaction.
- **Subagent memory — DECIDED.** Reviewer subagents: memory ON — accumulated knowledge of recurring violations improves the panel over time, and reviewers are read-only so remembered bias cannot write drift into code. Implementer subagents: memory OFF — an implementer accumulating its own remembered conventions is a drift vector, because its memory is not the contract and nothing keeps the two in sync; the per-worktree CLAUDE.md is its only memory, and that file is regenerated from the plan at each dispatch.

## Harness enforcement (rules move from prompt to harness — a prompt is a request, a hook is a guarantee)

The anti-drift mechanisms above are prompts until enforced; the model does not decide whether a hook runs. Every rule that must hold gets a harness binding, so violations block at write time, not at CI time.

### PreToolUse hooks (blocking; exit code 2 to block — the decision/reason return format is deprecated)
- Deny Write/Edit to `amk-types/**` from a subagent while a fan-out is in flight (frozen-types rule, otherwise honor-system).
- Deny Write/Edit outside the dispatched crate's path.
- Deny writes introducing `mail_parser::`/`mail_auth::`/`mail_send::`/`smtp_proto::` into amk-types, amk-core, amk-store (the P0 CI check, enforced at write time too).
- Deny `git reset` / `git checkout` / `git -C` / `GIT_DIR` from implementer subagents (a refactor agent inheriting Bash has, in a reported 2026 incident, run `git reset` and wiped uncommitted orchestrator work).
- Deny writes to the plan file and the registers from any subagent (orchestrator-only rule, otherwise honor-system).

**BUILT and under test** — `scripts/hooks/guard.sh`, `scripts/hooks/guard.test.sh` (count is
asserted by `scripts/plan-ledger.sh`, not transcribed here — a hand-copied count is a check that
silently stops checking, which this line did: it read 24 while the suite ran 32), both
directions: violations must block, legitimate work must pass). Three holes found by testing the
hook rather than trusting it, each now a regression case — worth recording because all three were
in a hook that *looked* correct:
- The comment exemption for rule 4 asked "does any line look like a comment?", which is true of
  every Rust file ever written, so the rule passed everything. It now strips comment lines first
  and greps the remainder.
- "Is this an implementer?" was decided from **CWD alone**, so a writer sitting in the primary
  checkout could write straight into a worktree and skip rules 1–3. Now either CWD or the target
  path being inside `.claude/worktrees/` makes it an implementer write.
- The scope rule (rule 3) was keyed on the **target**, which got both directions wrong: it never
  fired on an implementer writing an absolute path OUT of its worktree — nothing else catches that
  — and it DID fire on the orchestrator writing the dispatch contract IN, which is the one write a
  fan-out cannot start without. It is now keyed on the writer, where the question "may this agent
  write here?" actually lives.

**The limitation became an observation, and then a fix.** A subagent inherits the parent's cwd, so
rule 3 binds only when the agent's shell is actually inside its worktree. That was recorded as a
theoretical gap and then immediately demonstrated: the dispatched `amk-store` implementer wrote
`scripts/check.sh`, outside its `.amk-scope`, and the scope rule never fired. **CWD cannot identify
a subagent.**

So rule 0 stops trying. While `.claude/fanout.lock` exists, `crates/amk-types/**`, the plan, and
`scripts/hooks/**` are frozen for **everyone including the orchestrator** — which is rule 2 as
literally written ("a type change stops all parallel work"). `scripts/hooks/**` joins the frozen
set because a subagent that can edit the guard can disable every other rule. The lock is created at
implementer dispatch and removed at merge; read-only review panels do not need one.

**A contract freezes when its worktree is created.** The orchestrator created the id-safety
worktree, then rewrote that contract on `main`, and the worktree never saw the rewrite — so an
implementer worked a full round against a superseded document, and every "gap" it reported was real
against the current one. Nothing in this plan forbade that ordering and nothing detected it. If a
contract must change while its worktree is live, rebase the worktree and re-brief the agent
explicitly; never edit in place and assume it propagated. Enforced by
`hygiene-worktree-contract-fresh`, which diffs every live worktree's contracts against the primary's.

This is not a hypothetical guard against a careless subagent: during this session the orchestrator
was about to add `ApiKey` to `amk-types` "quickly" while an implementer was still in flight, which
is precisely what rule 2 forbids. The identity-free rule catches the person writing it.

Residual, honest: an out-of-crate write to a *non-frozen* path still depends on the orchestrator
reviewing the returned diff before merging. The hook narrows the honour system; it does not remove
it.

### Stop hook — deterministic phase gate
A Stop hook runs a script and blocks the turn from ending until it passes: failing tests, unformatted/unlinted tree, or a shape-provenance violation all block. **Noted explicitly: Claude Code overrides after 8 consecutive blocks — a Stop hook is a strong gate, not an absolute one; the check must be fast and terminating, and CI remains the authoritative gate.**

**DECIDED 2026-08-15 by the user — there is no CI layer. ~~CI remains the authoritative gate~~ is
struck: nothing runs on the forge.** `./scripts/check.sh` plus the PreToolUse and Stop hooks are
the only gates, and the departure is recorded here rather than left as an open item. What that
costs, stated rather than implied: every gate in this project now runs on the same machine, under
the same session, as the agent it is gating — the Stop hook is overridden after 8 consecutive
blocks, hooks and permissions are local files an agent with write access could edit, and a merge
pushed without running the check has nothing downstream to catch it. The mitigations that remain
are the ones already built: `scripts/hooks/guard.sh` freezes `scripts/hooks/**` during a fan-out
so a dispatched agent cannot disable the guard, and `scripts/plan-ledger.sh` fails the build on a
due-but-unmet obligation. Neither is independent of this machine. Revisit if the repository ever
takes contributions from anyone but the orchestrator.

### Permissions layer
`.claude/settings.json` with explicit allow AND deny rules per subagent role, plus `--allowedTools` scoping for any unattended run. The deny list is recorded explicitly — never rely on the `tools:` frontmatter alone. Hooks complement permissions, CI, and branch protection; they replace none of them.

**CORRECTED 2026-08-15 — the per-role deny lists were written in a key the runtime does not read, and the agents they were written in were not registered at all.** Each of the five `.claude/agents/*.md` carried a `permissions: { deny: [...] }` block; `permissions:` is a **settings.json construct and is not valid agent frontmatter**, so those rules bound nothing. Dispatching any of the five failed with *"Agent type not found"*, with only the built-in agents listed. A minimal probe agent using nothing but `name`/`description`/`model`/`tools` **also** failed to register, which isolates the second and larger fact: **agent definitions are read at session start and are not hot-reloaded.** Consequence, stated plainly: every dispatch made in that session ran under the default model, effort and tool set rather than the per-role assignments this plan specifies, and nothing inside a dispatch can observe that. The five files now use only keys with positive evidence for Claude Code 2.1.233 (`name`, `description`, `model`, `tools`, `disallowedTools`) and take effect at the next session start. `memory:` is deliberately absent from all five — the key is unverified for this version and an unsupported key is what cost registration, so the plan's reviewers-ON/implementers-OFF decision is currently **unbound and carried as a named ATTEST line in the ledger**, not quietly assumed.

Both ledger checks covering this were passing while it was broken, and both failed for the same reason: **they matched strings rather than the thing that binds.** `harness-permissions-per-role` grepped for the literal `deny:`, which an inert key satisfies; `mem-subagent-memory` grepped for `^memory:`, which an invalid value satisfies. They now assert `disallowedTools` as a whole field and allowlist the permitted frontmatter keys, so an unsupported key fails the build instead of silently deregistering an agent. The whole-field part was itself found by mutation, not by reading: the first fix grepped for `Edit` and reported MET on `disallowedTools: Write, NotebookEdit`, because `Edit` is a substring of `NotebookEdit`.

### Evidence over assertion
Per Anthropic's best-practices guidance, agents show evidence rather than asserting success. Added to every dispatch contract: **"Report the command you ran and its actual output. 'Tests pass' without the output is not a report."** — the P-1 fixture discipline applied to code.

### Hook hygiene
Hooks are arbitrary code running with the user's privileges: deterministic, fast, scoped, versioned, tested, runnable outside Claude Code. Never expose secrets in hook output, transcripts, or logs — DKIM key material passes through this project.

## Self-audit — three registers

Former items 8 (IMAP gap — user decision, recorded in Full parity) and 14 (content XOR url — recorded resolution in Gathered evidence) are deleted from tracking: resolved, no residual.

### Obligation ledger — `scripts/plan-ledger.sh` (added 2026-08-15 after an audit found 11 skipped steps)

The plan is long and its obligations sit inside prose, so "did we do that?" was answered from
memory and answered wrong. An exhaustive audit — three readers extracting every gate, deliverable,
mechanism, process rule and register commitment, then verifying each against the repository — found
**eleven skipped**, including one the orchestrator had *masked with its own record*: Register C3's
required code change was queued for the `amk-store` merge, and a `DEFERRED` entry in
`amk-types/tests/fixtures.rs` re-dated the same obligation to "P2", so the fixture-coverage tripwire
passed while `amk-core` shipped behaviour fixture 21 disproves.

**The rule this establishes: an obligation may be recorded in exactly one place.** A second record
of the same commitment is not redundancy, it is a place for the two to disagree — and the one that
disagrees quietly is the one that wins.

`scripts/plan-ledger.sh` is now that single place, run by `scripts/check.sh`. Machine-checkable
obligations are checks that fail when unmet **and due**; obligations that cannot be machine-checked
are listed as explicit attestation lines, because an omitted check reads as a passing one. Counts,
versions and file inventories are asserted there rather than transcribed into prose anywhere else.

### Register A — OPEN

**Standing rule: this register reaches zero when every item has been closed by an observed fixture. An item may leave A only by (i) a fixture landing, (ii) moving to B as a resolved requirement, or (iii) moving to C with an isolation strategy. Nothing is deleted from A without one of those three.**

**BLOCKING — needs user input BEFORE P-1 starts** (ask at kickoff, not mid-run): an Outlook.com/Hotmail address the user controls + willingness to mark the test mail as junk there (feeds A11/item 17).

### P-1 STATUS: **COMPLETE** (2026-08-15). Register A is empty of blocking items; only A2's confirmation tail runs on its own clock.
All 18 fixtures on disk in `reference/fixtures/` (+ `16-threading-matrix/`). Conformance harness built and self-test-validated. Probe teardown ledger at `00-probe-teardown.txt` (capture webhook + amk-probe2 deleted; pod/inbox/500-sink webhook intentionally live until the A2 tail completes, with the exact final-teardown commands recorded).

**The six findings that changed the build** (each supersedes a prior assumption):
1. Threading = strict RFC Message-ID chain, per-inbox; **subject is never a grouping key** (18 msgs → 17 threads; only the In-Reply-To pair merged). Killed the subject+correspondent-fallback design.
2. Error shape asymmetry is **real**: auth-layer → bare `{"message":…}` 401/403 even for a well-formed invalid `am_` key; app-layer → full envelope. `validation_error.errors[]` entries are `{code, path[], message}`.
3. Inbox collision = **`already_exists` HTTP 403 + `suggestions[]`** (both the docs' 409 and the SDK's 422 were wrong).
4. Unauthenticated mail is stored+labeled+webhooked but **excluded from `/messages` and `/threads` list endpoints entirely** — reachable only by GET-by-id or webhook.
5. `download_url` = CloudFront signed URL, **~1h TTL measured**, **403 `AccessDenied` after expiry**; page tokens are **base64(JSON keyset cursor)**; `message_id` is the SES angle-bracket Message-ID; `event_id` has **two formats** (UUID and 32-hex); live responses emit `organization_id`/`pod_id`/`smtp_id` beyond the SDK types.
6. DKIM keys export from the **running** Stalwart → **no cutover downtime**; `smtp-proto` is parser-only (amk-ingest owns the SMTP state machine); `mail-auth` needs **DER** keys, so the PKCS#1 PEMs get a conversion step.

### P0 STATUS (in progress) — `amk-types`, `amk-core`, `amk-store` GREEN and GATED; `amk-http` next

Write order position, updated 2026-08-16: `amk-types` ✅ → `amk-core` ✅ (gate met at a74ea3e) →
`amk-store` ✅ across **four** dispatches — the base crate, api-keys, id-safety, and
`inboxes::update` + control-plane text guards (3d3e1c9) — → **`amk-store` fifth dispatch, the
`amk-http` prerequisites** (`.claude/contracts/amk-store-http-prereqs.md`) → `amk-http`. 352 tests
green on `main`; Register C3 applied to `amk-core::threading`; the counts below are historical.

**Five amk-store dispatches, four of them created by the same check.** (The fifth,
`amk-store-http-prereqs`, merged at `8a14e63`: 208 tests, three review lenses, one correction
round. It found six of the 25 operations with no paginated persistence path, `pods::delete` unable
to express fixture 22's `cannot_delete`, an inbox holding an inbox-scoped api key undeletable at
all — `23503` on `api_keys_inbox_id_fkey`, reachable in P0 with no mail in the system — and the
minted-key constants contradicted by fixture 23.)

**`amk-http` MERGED 2026-08-16** (`3429ebb`, 17 commits, 4115 lines, 87 crate tests / 493
workspace). Three lenses plus one correction round. **The finding worth carrying: the two
get-by-id handlers checked permission *after* the store lookup, so a credential holding no
`inbox_read` got 403 for an inbox that exists and 404 for one that does not — and `inbox_id` IS
the email address, so that is a directly guessable existence oracle on a multi-tenant public API.**
Two independent readers called that ordering correct, including a review lens that verified the
decision clean, because the code's own comment argued "a 403 must never confirm a foreign pod
exists". Both had it backwards: checking permission *first* satisfies that concern too — the 403
is emitted before any lookup, so it confirms nothing — while closing the oracle scope-first opens.
Fixed `[INFERRED]` (no fixture observes the reference API's answer for that combination) with the
test that distinguishes the two orderings, which none of the original 83 covered. **A reviewer
agreeing with the code is not the same as the code being right; ask what each ordering discloses,
not whether the stated rationale is coherent.**

Also merged with it: `shape-provenance.sh`'s `PROTECTED` set extended to `amk-http`, **after** the
merge — changing a gate while its worktree is live yields two verdicts for one tree, the same
hazard as editing a contract in place.

**`amk-cli` MERGED 2026-08-16 (`e8ee5f5`, 3 commits, 2002 lines, 546 workspace tests) — and with it
P0's SDK gate is MET for the first time.** `reference/fixtures/24-p0-gate-sdk-authme.txt` captures
the unmodified official `agentmail==0.5.9` Python SDK calling `auth.me()` against `amkd --role api`
on localhost and receiving an `Identity` whose `scope_id == organization_id` and
`scope_type == "organization"` — matching fixtures 01 and 22. The gate had read PENDING since P0
began because nothing served HTTP: `amk-http` shipped a `router()` and no binary bound it.

**The finding worth carrying: a passing gate proved less than it looked, and the same coincidence
hid a live mutant.** `serve_api` builds `AppState::new(pool, config)`, and mutating that to
`AppConfig::default()` — silently discarding the operator's `AMK_PRIMARY_DOMAIN` and
`AMK_PRODUCT_NAME` — survived every test in the crate. It would also have survived the gate
capture, because that run had both variables **unset**, which makes `AppConfig::default()` and the
real config identical. Since `primary_domain: None` makes inbox creation without an explicit
`domain` fail closed, the mutant's real signature is an operator who configures the domain
correctly and still gets a failure on every `POST /v0/inboxes` that omits one. Found by the
test-adequacy lens, not by the gate. **An acceptance capture taken with a value unset does not
exercise the path that reads it** — set the configuration a gate is meant to prove, or the gate
agrees with the bug.

Also from that round: `doctor`'s connected-but-`migration_status`-failed arm had no test, so a bare
`{e}` there would have leaked a DSN past `redact.rs` and passed; the implementer reached it by
extracting a pure `migration_status_line(Result<…>)` rather than contorting the code to force a
live failure, which is the right call and is recorded as such. And a `config` test that set and read
through the same constant could not fail on a rename — the self-comparison shape again, third
appearance.

**One contract defect, resolved after the fact rather than before it:** the prohibition "No SQL in
this crate" was unconditional, and the integration suite needs `CREATE DATABASE` to give `amk init`
a database `organizations::exists` reports as empty. The implementer resolved that silently instead
of escalating; a review lens caught it. The resolution stands — DDL provisioning of the *container*
a schema lives in is not persistence routed around `amk-store`, and no public function could ever
have covered it, since both `connect` and the migrator operate inside an existing database — but
the carve-out is now written into the contract and into the code, because a prohibition with no
stated exceptions is one the next reader applies without them.

**Three lessons from that dispatch, each bought:**

1. **A test that asserts the old behaviour is a site, exactly as a call site is.** The derivation
   script scoped itself by grepping `src/`, and the pre-dispatch review found two blocking misses
   in `tests/`: an assertion pinning `pods::delete`'s current failure mode, and two hardcoded
   `VISIBLE_LEN = 8` boundaries that would have asserted something *false* under the new constant
   rather than failing loudly. Scope derivation now enumerates test assertions and constant uses,
   not only call sites and error catches.
2. **When a decision changes during contract revision, every list derived from it is re-read.** The
   cursor decision was rewritten after the pre-dispatch review; the assigned-edge-case list derived
   from it was not, and went out naming a field the same contract forbade. The implementer was
   right not to invent it.
3. **Randomised test fixtures can make a test unfalsifiable in a way that looks like coverage.**
   Three tiebreak tests seeded rows with random ids, so whether a dropped `ORDER BY` tiebreak
   surfaced was roughly a coin flip — measured at 3 of 10 runs by a review lens, and 0 of 5 for the
   earliest version, which could not fail *by construction* because two rows and `limit: 2` never
   cross a page boundary. Found by the implementer's own mutation pass, not by a reviewer. This
   joins the self-comparison and the iterate-your-own-copy shapes in the "a test that has never
   failed is not evidence" family: **a test whose seed data is random is a test whose failure is
   random.**

**Four amk-store dispatches, three of them created by the same check.** The write order's real
product is the pre-dispatch pass: each time the `amk-http` contract was checked against
`amk-store`'s *actual public surface*, it found the next crate unbuildable, before an implementer
had to invent a storage shape to proceed. api-keys (no table, no repository, no hash) →
`inboxes::update` (no `update` function at all, and `get`/`delete` pinning `organization_id` only,
a cross-pod read and delete) → the four findings now in `amk-store-http-prereqs.md`. None was found
by re-reading a contract.

Two live probes were run to settle questions the 25 operations left to invention rather than let an
implementer guess — `reference/fixtures/22-org-mount-and-delete-semantics.txt` (org-mount pod
resolution; `DELETE` pod 204 / inbox 202, both documented 200; non-empty pod → 409 `cannot_delete`,
a code appearing **zero times** in `openapi.json`) and `23-inbox-defaults-and-key-shape.txt`
(generated username shape; `DELETE /v0/api-keys` 204, making `openapi.json` 0-for-3 on DELETE
statuses; no GET-by-id route for api-keys; and the real minted-key shape, which **contradicts** the
`[ASSUMED]` `SECRET_LEN`/`VISIBLE_LEN` this crate shipped). Fixture 23's probe printed a plaintext
secret to the transcript before redacting — a defect in the probe script, not the API; the key was
deleted (204) in the same run and its absence re-verified. **Any future probe touching
`CreateApiKeyResponse` redacts at capture, not after.**

The first `amk-store` dispatch was deliberately **narrower than its contract file**: migrations, pool and
error mapping, the four P1 control-plane repositories, keyset pagination, and the message/thread
read path carrying the admission predicate. Blob store, FTS/search, signed download URLs, the jobs
table, and the idempotency layer are a **second dispatch** — named as deferred so the omission is a
decision rather than a gap, and so one returned diff stays reviewable.

**AMENDED 2026-08-15 — that deferral list had a hole, and the hole blocks the next crate.
`api-keys` was named nowhere: not in the first dispatch, not in the deferral list.** It surfaced
when the `amk-http` contract was checked against `amk-store`'s actual public surface before
dispatch: `amk-http`'s auth layer requires "O(1) lookup by key id then a constant-time verify of an
argon2id hash", and there is no `api_keys` table, no repository and no hash in the crate. `amk init`
("default org+pod, root key shown once") has the same dependency. So **`amk-http` does not start
next**; the amk-store second dispatch does, scoped to api-keys, contract at
`.claude/contracts/amk-store-api-keys.md`. The write order is what caught this — before an
implementer had to invent a storage shape to proceed, which rule 3 forbids.

Three decisions settled in that contract rather than left to the implementer, each because it is
security- or conformance-shaped: argon2id and the hash live **inside amk-store**, so the hash never
crosses a crate boundary and exactly one place owns the parameters; `authenticate` **must not
write** — `used_at` is a separate call, keeping the auth hot path read-only; and our minted keys
must **never begin `am_eu_`**, because the official node SDK routes that prefix to AgentMail's EU
host `[SPEC:sdk environments.ts, Client.ts:80]` and would leave our base URL entirely — the one
thing V1 acceptance forbids. The `permissions` column is nullable **and the nullability is
load-bearing**: SQL `NULL` is the absent object (grants everything), `'{}'::jsonb` the
present-but-empty one (grants nothing), and collapsing them is a privilege bug in both directions.

`amk-types`: 60 tests. The `reference/fixtures/` captures are now wired as **actual regression
tests** (`crates/amk-types/tests/fixtures.rs`) that read the files, rather than hand-transcribed
values in doc comments — verified by poisoning three fixtures and confirming each fails.
`every_fixture_is_either_asserted_or_explicitly_deferred` fails when a capture is added without
being wired in, so the gap is visible rather than assumed. Harness enforcement is live:
`scripts/hooks/guard.sh` (19 tests) blocks frozen-type edits, out-of-scope writes, plan edits and
stalwart-labs leakage at write time; `scripts/check.sh` is the single verify command.

**`amk-core` review found three blocking defects — the fan-out worked exactly as the anti-drift
section predicted it would need to:**
1. **Scope privilege escalation.** `from_identity` rejected a *narrow* scope_type missing its id but
   silently discarded *narrowing* ids carried on a wider one, so an inbox-bound credential with a
   stale `scope_type` read the whole organization.
2. **Permissions failed open on subscribe.** A key holding only `webhook_create` could subscribe to
   `message.received` and read every inbound body through the webhook while `GET /messages/{id}`
   denied it — and a test *asserted* that behaviour, locking it in.
3. **A fan-out collision.** `labels.rs` and `permissions.rs` independently implemented
   `is_visible`/`retain_visible` with **opposite verdicts**, both green. Root cause: `amk-types`
   owned no `ApiKeyPermissions`, so two isolated workers each invented one. Fixed at the root
   (`amk-types::api_key`), and the re-dispatch gave the coupled pair a **single owner** — splitting
   that decision across two contexts is what produced it.

**New evidence captured during P0** (each settled a question that had been escalated or flagged,
rather than guessed):
- `18-inbox-case-normalization.txt` — see B4. Also caught `limit_exceeded`'s extras (B5).
- `19-message-label-patch-gate.txt` — see B6. **This one overturned a reading the orchestrator and
  two reviewers had all agreed on from the OpenAPI descriptions**, and is the clearest case yet for
  the rule that the live capture beats the spec text.
- `20-search-and-label-precedence.txt` — see B7. Search does not hide restricted mail.

**Round 2 of review (three lenses over the merged crate).** The test lens ran **30 source mutations
and killed 24**, confirming the round-1 fixes are genuinely pinned rather than merely present. Six
survivors and two independent confirmations of a real leak:
- `redact_thread` repaired eight aggregates and never `item.labels`, so a redacted thread still
  named the hidden message's `spam` label — the exact disclosure `LabelDenial::label` is
  `pub(crate)` to prevent. Two lenses each built a scratch crate and reproduced it.
- `Resolved::into_ready()` had zero tests; a one-line change reopens the mount-probe fail-open that
  round 1 had just closed, through a sibling function the regression test never calls.
- The inbox case-folding pin was a self-comparison (expected value built from the same lowercase
  constant), so dropping `.normalized()` broke nothing.
- `ALL_EVENTS` was hand-copied in amk-core and its totality test iterated the copy — a tripwire that
  iterates the thing it is meant to catch drifting cannot fire. Fixed at the root: `EventType::ALL`
  in amk-types, with a wildcard-free match that fails to **compile** if a variant is added.
- `WIRE_NAMES` was pinned only in the forward direction, so a field added to `ApiKeyPermissions`
  would be invisible to the child-key escalation bound.
The lesson recorded for later phases: **a test that has never failed is not evidence.** Mutation
testing found in one pass what two review rounds of reading did not.

**Close-out (2026-08-15, commit a74ea3e) — `amk-core` GATE MET.** An independent verifier re-ran
each of the ten findings as a mutation against a scratch copy and confirmed all ten dead: the
`redact_thread` label strip (deleting it → 3 failures), the three-mode `LabelAccess` (mutating
Search to re-apply the include flag → 4 failures; mutating the List arm → 7), `EventType::ALL`
(two-layer compile tripwire reproduced at `event.rs:71` then `permissions.rs:176`),
`into_ready()`'s `Result`, the case-folding pin (now a raw-`&str` comparison that fails
`"AMK-Probe@AgentMail.To"` vs `"amk-probe@agentmail.to"`), the per-field violation index, and the
four redaction aggregates. `./scripts/check.sh` PASS. **It also found one new survivor, introduced
by the fix for finding 1** — and this is the pattern worth carrying forward, because the fix was
itself reviewed and green:
- Deleting `redact_thread`'s `if !hides_a_member { return Redacted; }` early return left the suite
  at 117/117. Not behaviour-neutral: on a labels-only redaction it made the function invent
  aggregates (`received_timestamp` `None` → `Some(…)`, `senders` losing a member), the exact harm
  that early return's own comment names. The gap was in the test, not the code — it built its
  thread from a helper whose aggregates already agreed with what recomputation produces, so "no
  aggregate is re-derived" was unfalsifiable. Fixed by seeding two values recomputation *would*
  change; the mutant now fails.
- Second, smaller: the fixture-20 test quoted the capture's counts in a comment, so file and test
  could drift apart silently. It now reads the fixture at test time (poisoning `count=1 STILL
  FOUND` → `count=0` fails it). Same treatment the amk-types suite already gets — **a fixture
  quoted in a comment is not a fixture under test.**

### P-1 EXECUTION DETAIL (live against api.agentmail.to via sdxd; throwaway pod 9047724b-…, inbox amk-probe@agentmail.to)
- **CLOSED (fixture on disk):** A13 `01-auth-me.http` · A1 `03-id-formats.http` (message_id = SES angle-bracket `<…@email.amazonses.com>`, header-derived; thread_id UUID; **event_id has TWO formats** — UUID for delivered, 32-hex for sent; extra emitted fields smtp_id/organization_id/pod_id) · A4-pagination `04-pagination.http` (**token = base64(JSON keyset cursor)** {message_id,inbox_id,timestamp}) · A8 `05-error-catalog.http` (**asymmetry REAL**: well-formed-invalid am_ key still returns bare `{"message":"Forbidden"}` 403; app errors get full envelope) · collision `05` (**`already_exists` HTTP 403 + `suggestions[]`** — NOT resource_taken/409, NOT 422; P1 gate updated) · A9-partial `09-event-payloads.txt` (sent+delivered) · A6 `10/11/12` · A7 `14`.
- **CLOSED — A2** `07-webhook-retry-curve.txt`: **stock Svix schedule, NO override, NOT truncated at 5.** Across 4 independent message.sent events the gaps track immediate → +5s → ~+5m → ~+30m → ~+2h (jitter band ±18%, e.g. the 5m slot spans 4m12s–5m53s); svix-id stable within each chain. **Two chains reached a 6th attempt in the 5h slot** (+4h20m32s, +4h15m25s), which is what kills the truncation hypothesis — the residual question, not the confirmation. 10h/10h tail still accruing, non-blocking. · **A3 CLOSED** `06-download-url-expiry.txt` — CloudFront signed URL, ~1h TTL measured; 200 before expiry, **403 AccessDenied (CloudFront XML) after**.
- **CLOSED — A12** `15-compile-spike.txt`: spike compiles (cargo build exit 0), all 11 pins resolve, 8 API-fit corrections captured (folded into the crate-pins section above; notably smtp-proto is parser-only and mail-auth DKIM needs DER keys).
- **CLOSED — A4** `10b-dkim-extraction.txt`: **outcome (a)** — live DKIM keys exportable from the RUNNING Stalwart (`GET /api/settings/list?prefix=signature.` or `stalwart-cli … list-config signature.`), offline RocksDB read is fallback only → **NO cutover downtime constraint** (doc-corroborated, admin-cred not exercised read-only; high confidence).
- **CLOSED — A10 (a–f)** `16-threading-matrix/`: strict per-inbox Message-ID chain, subject not a grouping key (see Threading row). **A10(g) downgraded**: since subject NEVER groups at n=1 across all prefix/dup/empty cases, the T+30d window bisect only tests whether time changes subject-only behavior that already doesn't group — **optional confirmation, non-blocking, may be skipped**; script `g-window-bisect.sh` written but not launched. Uncovered dims (documented): In-Reply-To vs References alone, multi-hop chains, cross-thread merge, authenticated mail, n>1.
- **CLOSED — A14** `09b-unauthenticated-variant.txt`: SPF=none+no-DKIM → stored, labels `[received,unread,unauthenticated]`, `message.received.unauthenticated` fires (payload + Authentication-Results captured); SPF hardfail (`-all`) → accepted at SES gateway (250) then dropped/quarantined, not surfaced. **Finding: unauthenticated mail is EXCLUDED from `/messages` and `/threads` list endpoints (count 0 even with `?labels=`) — retrievable only by GET-by-id or webhook.** Side effect: throwaway `amk-probe2@agentmail.to` created (referenced by fixture e).
- **CLOSED since:** A5 `13-source-ip-echo.txt` — **POSITIVE**: external client IP 45.233.219.186 reached the pod unaltered through an ETP-Local NodePort; MetalLB approach works, **Multus /32 fallback NOT needed**; throwaway ns cleaned up.
- **CLOSED — A11** `17-message-complained.txt`: Gmail attempt produced nothing (no per-message FBL, as predicted); **Outlook (nathant1902@outlook.com) reported-as-junk → `message.complained` captured in ~30s**. Live values: `complaint.type="abuse"`, **`sub_type` absent** (omit when empty), `event_id` UUID, complaint object emits extra organization_id+pod_id. FULLY CLOSED.
- **P-1 DELIVERABLE DONE:** dual-target conformance harness (`conformance/dual_target.py` + `manifest.json` + README) — **validated** 2026-08-15 (`--self-test`: 11 read-only GETs, 0 structural diffs, exit 0). Structural shape-diff (keys+types, not values); gates phases P1–P5.

(Honest end-state stated once, after A13, with all items counted.)

A1. `event_id` and page-token exact wire shapes (message_id already `[SPEC:docs attachments]`, needs live confirmation in the same probe).
    closes when: real `evt_...` id, a real `message_id` from a fetched message, and a real `next_page_token` from a >1-page listing are captured from live resources.
    probe: P-1 items 3–4
    fixture: reference/fixtures/03-id-formats.http, reference/fixtures/04-pagination.http

A2. **CLOSED** (2026-08-15 13:17Z) — AgentMail does **not** override the Svix retry schedule, and does **not** truncate it at 5 attempts. Early curve: 4 chains × 5 attempts, every gap assigned to its stock slot (immediate, 5s, 5m, 30m, 2h) with a smallest margin of 1.9× over the runner-up, unanimous across three distance metrics; an independent audit fit the data against alternative schedules an implementer might configure and stock beat them by 100–1000×. **The truncation question — the one real residual — is answered: two independent chains fired a 6th attempt** (`msg_3HwIMgrDhRnHtUDw6GAIE2nWCtj` +4h20m32s, `msg_3HwLQPnrB4N72YNkXNeS7A454ox` +4h15m25s, both landing in the 5h slot). Consequence: our engine keeps all 8 attempts, so `message.attempt.exhausted` timing and the 5-day auto-disable rule stand as planned. Caveat retained rather than dropped: `[SPEC:svix]` is the **OSS** `config.default.toml` and AgentMail most likely runs Svix Cloud, so the evidence says "the observed schedule matches the stock schedule", one step short of "they set no override" — a distinction with no build consequence, since we implement the observed behaviour either way. The 10h/10h tail is pure confirmation and blocks nothing; the sink and its webhook stay up until it lands or is abandoned, per `00-probe-teardown.txt`.
    probe: P-1 item 7
    fixture: reference/fixtures/07-webhook-retry-curve.txt

A3. **CLOSED** `06-download-url-expiry.txt` — full lifecycle observed on the raw-message URL: `download_url` is a **CloudFront signed URL** on `cdn.agentmail.to` (`?Expires=<unix>&Key-Pair-Id=&Signature=`), `expires_at` is **exactly ~1h** after issue (measured, not assumed), **GET before expiry = 200**, **GET ~150s after expiry = 403 with CloudFront XML `<Error><Code>AccessDenied</Code>`**. amk's signed-download endpoint must mint ~1h URLs and return 403 on an expired/invalid token.
    RESIDUAL (folded into open item 15, non-blocking): the small-vs-large **attachment** inline-bytes-vs-URL threshold (~5.95MB) was not exercised — the raw-message path always returns a URL. Confirm when P2 implements attachments.

A4. DKIM extraction mechanism — pulled forward into P-1 (NOT migration prep: this is the one finding that could impose a cutover downtime constraint, and it must be known months early). Attempt the Stalwart admin API / settings export against the running server, read-only, during P-1. Outcome (a): admin API can export the signing keys → no downtime constraint, A4 closes. Outcome (b): it cannot → offline RocksDB read required → Stalwart stopped or file-level copy → downtime-window constraint recorded into P6 now. Either way A4 closes in P-1. Key material never enters the transcript or fixture — the fixture records mechanism, endpoint, status, and match-verdict metadata only.
    closes when: the export mechanism is determined against the live server (works / does not work), with the P6 consequence recorded.
    probe: P-1 item 10 (same SSH session class as items 10–12)
    fixture: reference/fixtures/10b-dkim-extraction.txt

A5. Source-IP preservation through MetalLB/ETP-Local on this cluster (kernel prerequisites `[TESTED]`; end-to-end not run).
    closes when: an external curl (vantage 45.233.219.186) through an ETP-Local NodePort shows the pod observing that client IP unaltered — or the blocking layer is captured by name. A negative result CLOSES A5 (the observation is the answer) and activates the P6 step-2 fallback decision — routed /32 via Multus for the smtpd pod only, plus the exposure.toml call to the user. A5 does not stay open on a negative outcome.
    probe: P-1 item 13
    fixture: reference/fixtures/13-source-ip-echo.txt

A6. Server-evidence fixture writes (P-1 items 10, 11, 12) — observations complete this session (DKIM location; CP relay `m.appsynergy.io:587` inert; dependents table), preserved verbatim in the subagent report; writes only.
    closes when: the three preserved raw captures are written to disk.
    probe: P-1 items 10–12
    fixture: reference/fixtures/10-dkim-keys.txt, reference/fixtures/11-cp-smtp-relay.txt, reference/fixtures/12-stalwart-dependents.txt

A7. IMAP crate survey — observation complete, fixture write only.
    closes when: the preserved survey data is written to disk.
    probe: P-1 item 14
    fixture: reference/fixtures/14-imap-crate-survey.txt

A8. Error-shape asymmetry: does a well-formed-but-unknown `am_` key return the full envelope with `code:"unknown_api_key"` (docs) or the observed bare `{"message":"Forbidden"}` (probe key likely gateway-rejected as malformed)? **P0's error-shape implementation waits on this.**
    closes when: the response to a realistic-format invalid `am_` key is captured.
    probe: P-1 item 5
    fixture: reference/fixtures/05-error-catalog.http

A9. Inbox collision code: `resource_taken` 409 (docs) vs 422 (SDK-derived guess). P1 builds whichever is observed.
    closes when: a duplicate-username inbox create against the throwaway pod is captured.
    probe: P-1 item 5
    fixture: reference/fixtures/05-error-catalog.http (distinct capture in the same file)

A10. Their threading rule set for header-less mail (moved from C — the matrix makes it observable; we control the sender). Full-algorithm recovery is still not claimed: the report must name uncovered dimensions, and the trait-in-amk-core isolation (~~JWZ default~~ → strict Message-ID chain default) stays regardless. **PARTIAL-CLOSING**: cases (a)–(f) close within P-1; case (g) — the window bisect — is a long tail by construction with an explicit closure horizon of **T+30d from launch**, launched on day one of P-1 so the clock starts immediately; non-blocking because threading is P2 and this path fires only for mail with no In-Reply-To.
    closes when: (a)–(f) fixtures land [in P-1] and (g) completes at T+30d [on its own clock].
    probe: P-1 item 16 ((g) as background scheduled probe from day one)
    fixture: reference/fixtures/16-threading-matrix/

A11. `message.complained` trigger/timing and live `type`/`sub_type` values (moved from C — the "likely unobtainable" call was wrong: backend is SES, which is enrolled in ISP FBLs; wire shape already `[SPEC:openapi]`, verified this session against the downloaded spec).
    closes when: a real complaint payload from a junk-marked Outlook/Hotmail delivery is captured at the webhook sink — or the recorded attempt times out and this moves to C with the attempt documented.
    probe: P-1 item 17 (needs user's Outlook/Hotmail address + junk-marking at execution)
    fixture: reference/fixtures/17-message-complained.txt

A12. Crate API fit — pinned versions exist on crates.io, but the APIs we assume (sqlx query construction, axum 0.8 handler signatures, mail-auth verdict types, rmcp tool registration + streamable-HTTP server) are unverified.
    closes when: a minimal spike compiles against each assumed API, with any mismatch recorded and the pin revised.
    probe: P-1 item 15
    fixture: reference/fixtures/15-compile-spike.txt

A13. auth/me Identity capture — observed 200 org-scoped this session, fixture write only.
    closes when: the preserved capture is written to disk.
    probe: P-1 item 1
    fixture: reference/fixtures/01-auth-me.http

A14. `message.received.unauthenticated` variant — live payload + labeling behavior. We control a sender that can fail authentication on purpose: swaks from the OVH box with a MAIL FROM domain whose SPF does not authorize that IP (and no DKIM), sent to a throwaway inbox subscribed with `label_unauthenticated_read`.
    closes when: the event payload and the message's `unauthenticated` label state are captured — or their gateway's rejection of the unauthenticated mail is captured instead (either observation is the answer; a rejection also closes A14).
    probe: P-1 item 9
    fixture: reference/fixtures/09b-unauthenticated-variant.txt

**P-1 item → register map (exhaustive; no item untracked):** 1→A13 · 2→exempt (user decision, recorded in Full parity — nothing left to observe) · 3→A1 · 4→A1 · 5→A8+A9 (residual catalog rows are `[SPEC:docs errors]`; captures corroborate) · 6→A3 · 7→A2 · 8→exempt (superseded by item 16) · 9→payload shapes already `[SPEC:openapi]`, captures corroborate; hard cases split to A11, A14, and C1 · 10→A4 (mechanism) + A6 (fixture write) · 11→A6 · 12→A6 · 13→A5 · 14→A7 · 15→A12 · 16→A10 · 17→A11.

**Honest end-state (restated with A1–A14): at end of P-1, Register A = A10(g) only** — every other entry incl. A12/A13/A14 closed, (g) on its stated T+30d clock. Anything else still open at that point is a P-1 failure, not an acceptable residual.

### Register B — RESOLVED → REQUIREMENTS (not open; each generates build work)

B1. `validation_error` carries `errors[]` of **`{code, path[], message}`** (path is a JSON-pointer array; each entry also has a `code`, refined from live capture `05-error-catalog.http`) → implement in **amk-types** (error envelope), **P0**. Also: the error envelope on some codes carries extra fields (e.g. `already_exists` → `suggestions[]`) — model per-code extras.
B2. Block-list entries auto-added from bounces/complaints/unsubscribes, `read_only`, undeletable via API `[SPEC:docs errors]` → `read_only` flag in **amk-types**; auto-add write path in **amk-ingest** (DSN/ARF feedback pipeline), **P5**.

B3. **Restricted-label visibility is ONE composed rule, owned by `amk-core::labels`** — resolved 2026-08-15 after the P0 review panel found `labels.rs` and `permissions.rs` had independently implemented the same gate with opposite verdicts (a fan-out collision: two isolated workers, one question). A restricted-labelled row appears in a **list** result only if the credential holds the gating `label_*_read` permission **AND** the request set the matching `include_*` flag; **get-by-id** needs only the permission. `permissions` exposes the permission half only, under a name that cannot be mistaken for the whole check. Root cause was that **amk-types owned no `ApiKeyPermissions`**, so each worker invented one — now fixed (`amk-types::api_key`, 36 flags generated from openapi.json, plus `label_read_flag` pairing). **Admission must be a storage-layer predicate**: post-filtering a fetched page leaves a gap, so `?limit=1` walked across the cursor returns `count:0` with a `next_page_token` on exactly the hidden rows, disclosing their number and their cursors. → **amk-store**, P1/P2.

B4. **`inbox_id` folds case** — `[TESTED]` fixture 18: `{"username":"AmkCase"}` is stored and returned as `amkcase@agentmail.to`, and `GET` resolves `AMKCASE@…`/`AmKcAsE@…` with 200. Every scope check, storage lookup, ACL comparison and thread-index key uses the ASCII-lowercased form (`InboxId::normalized`/`eq_normalized`). Exact comparison is a defect, not a conservative default: it diverges from upstream on precisely the input a caller controls. Closes the plan's own open question ("is `Foo@x.com` the same inbox as `foo@x.com`? decide, then test"). Not covered: whether the local part folds for **inbound SMTP** routing (RFC 5321 makes it case-sensitive) — a P2 question.

B5. **`limit_exceeded` carries per-code extras `resource`, `limit`, `upgrade_url`** `[TESTED]` fixture 18 — a third per-code extra after `already_exists`' `suggestions[]` and `validation_error`'s `errors[]`, confirming envelope extras are per-code rather than a fixed set. The quota is counted **organization-wide, not per pod**. AgentMailKit reproduces `resource`/`limit` (a self-hosted deployment may still impose a configured cap) and **omits `upgrade_url`**, with no plan or price in the `fix` string — the no-billing-surface rule applied deliberately, recorded so the omission is a decision rather than an oversight. → **amk-types** error extras, **amk-http**.
B6. **`system` and `restricted` are two independent axes** — `[TESTED]` fixture 19, which PATCHed every candidate label onto a live message. **System** (a client may not add or remove it) is exactly `{sent, received, bounced, scheduled}` → 400 `validation_error` "Cannot use system label: …". **Restricted** (hidden from list endpoints unless included) is `{spam, blocked, unauthenticated, trash}`. **Neither implies the other**: a client may freely set `spam` on a message, and `unread` is settable but not restricted. Restricted governs who may SEE a label; system governs who may SET one.
    - **This supersedes the plan's earlier reading** that the system-label restriction applies to *thread* PATCH only. That reading came from the OpenAPI descriptions (`type_threads:UpdateThreadRequest` says "Cannot be system labels"; `type_messages:UpdateMessageRequest` says nothing) and was independently reached by two reviewers **and accepted by the orchestrator into a dispatch instruction**. The live API gates messages too. Evidence beats the spec text — this is the clearest instance so far.
    - `scheduled` IS system: the implementer had classified it by inference and a reviewer flagged that as unevidenced; the inference was right and is now observed.
    - A request naming one system label rejects the **whole** mutation, not just the offending element.
    - `errors[].path` is `["add_labels", 0]` — mixed string/integer members, a JSON-pointer path, not a list of field names.
    - New label constant `complained`, observed on the message that drew the Outlook FBL complaint. Its system-ness is **unobserved** (not exercised by the probe).
    - Restricted-label list exclusion is about the **label, not its provenance**: a client-applied `spam` hid the message from list endpoints exactly as a pipeline-applied one does.
    → **amk-types** (`labels::{SYSTEM, RESTRICTED, is_system, is_restricted}`, done), **amk-core::labels** policy, **P1**.

### Register C — ACCEPTED UNKNOWNS (each states what was attempted / why closure is precluded)

C1. `domain.verified` live capture — SHAPE PINNED, only live behavior unobservable. Fixture `reference/fixtures/C1-domain-shape.txt` (written, 6 labeled blocks, all `[SPEC:*]` — never `[TESTED]`, no live observation exists by design under D1): verbatim Domain/DomainItem/VerificationRecord/RecordType/RecordStatus/VerificationStatus/DomainVerifiedEvent schemas + every `/v0/domains` path from openapi.json; both SDKs' typed models incl. node wire serialization; custom-domains + managing-domains docs pages; and the authoritative sequence `[SPEC:docs errors]`: GET /v0/domains/{domain} → returns the DNS records to add → POST /v0/domains/{domain}/verify — P5 implements exactly this, not a config-file or selector-driven model. **Known** `[SPEC:openapi + sdk + docs]`: the Domain object, DNS-record shape, `domain.verified` payload, and the GET-records → POST-verify sequence. **Unknown, unobservable under D1**: their real record VALUES, actual propagation/verification timing, and event ordering on a live domain. Isolation: emit-interface boundary; P5 exercises the full flow against our own implementation using the pinned shapes. Reopens only if the user reverses D1.

B7. **Label access has THREE modes, not two** — `[TESTED]` fixture 20. A spam-labelled message vanishes from `GET …/messages` and is **still returned by `GET …/messages/search`**, same inbox, same credential, same moment.
    - **list with include flags** → permission AND the matching `include_*` flag. Applies only to the **4 of 33** paginated GETs that carry those parameters (`/threads`, `/pods/{id}/threads`, `/inboxes/{id}/threads`, `/inboxes/{id}/messages`).
    - **search** → permission only; restricted mail **is** returned.
    - **get-by-id** → permission only (`[TESTED]` 09b).
    - Caught by the review panel as a rule generalised past its evidence: amk-core had documented "`include_*` on every list endpoint" and offered only list/by-id modes, which would have made restricted mail **unreachable by search for every credential that will ever exist**, with no parameter capable of turning it on. Probed rather than marked `[INFERRED]`, because a permanent impossibility is not something to carry as a hedge.
    - Caveat: the probe key was org-scoped and unrestricted, so only the *include-flag* half of search is observed. Treating search as permission-gated like get-by-id is the fail-closed reading. → **amk-core::labels**, **amk-store** query predicate, **P1**.
    - Same probe: **`remove_labels` beats `add_labels` on a message too** — that generalisation from the thread schema was correct and is now `[TESTED]`.

C2. **Whether a thread's labels are a strict union of its members' labels** — raised by the P0 review panel, unobservable from the fixtures we have. Every threaded message in `16-threading-matrix/` and in `09b` is homogeneously labelled, so a **mixed** thread (one `spam` member alongside clean ones) is entirely unevidenced. (A reviewer offered `16-threading-matrix/a.txt` as evidence that thread labels ARE a member union — the thread and both its members read `["received","unread","unauthenticated"]`. Checked: the two members carry *identical* labels, so that observation is consistent with a union rule but equally consistent with copy-from-root or any other rule. It does not discriminate, and C2 stays open.) It decides a real behaviour: whether `GET /threads/{id}` may return a thread whose aggregate labels look clean while `messages[]` and `message_count` include a restricted member. Isolation: the fail-closed choice is implemented — when any member is hidden, membership is filtered **and** the aggregates (`message_count`, `size`, `last_message_id`, `senders`, `recipients`, `preview`) are recomputed from what remains — confined to one function in `amk-core::labels` and marked `[INFERRED]`. Closes if a probe ever produces a mixed-label thread; needs a spam-classified message threaded to a clean one, which we cannot reliably induce on the live API.

C3. **CLOSED — and it reverses the choice we made.** `[TESTED]` `reference/fixtures/21-unbracketed-in-reply-to.txt`: a bare, unbracketed `In-Reply-To` **does** join the referenced message's thread. Three messages into `amk-probe@agentmail.to`, each with a *different* subject so subject could never be the explanation: ROOT (`<amkc3-root-ccc7baa7@appsynergy.io>`, no linkage header) → thread `886a4b21-…`; BARE (`In-Reply-To: amkc3-root-ccc7baa7@appsynergy.io`, no brackets, no `References` at all) → **same thread**; CONTROL (same value bracketed) → same thread. `message_count:3`. The control is what makes the run self-validating — had it not joined, the probe would have been broken and BARE's result meaningless.
    The decisive detail is not the thread_id but the field: **BARE's API-level `in_reply_to` comes back `"<amkc3-root-ccc7baa7@appsynergy.io>"` while `headers.In-Reply-To` (the raw received header) stays unbracketed exactly as sent.** AgentMail normalises the parsed value before matching. That is direct evidence of the mechanism, not an inference from the outcome.
    **REQUIRED CODE CHANGE, queued:** `amk-core::threading` currently trims CFWS only and asserts in `an_unbracketed_linkage_header_is_not_coerced_into_a_match` that a bare addr-spec must *not* match. Both the behaviour and that test are now wrong and must be inverted — normalise a bare addr-spec to the bracketed form before thread-matching, and keep the normalisation confined to linkage headers (the *stored* `message_id` is still whatever arrived). Not applied immediately because `amk-store` was in flight against `main` and it depends on `amk-core`; threading is P2, so the queue costs nothing. Apply at the `amk-store` merge.
    Not covered, named rather than implied: a bare value in `References` rather than `In-Reply-To`; multi-hop chains; a bare value with surrounding CFWS comments; a message whose own `Message-ID` header is unbracketed; malformed addr-specs; authenticated mail; n=1 (not repeated).

(Former C1 threading → A10 via the item-16 matrix; former C2 complained → A11 via the item-17 FBL attempt. The residual truth from old C1 survives inside A10: the matrix infers a rule set, it does not recover their algorithm — uncovered dimensions get named, and the trait isolation stays.)

Footnote (not an open claim): week-level effort estimates were removed as false precision; phase order is the commitment, durations are not.
