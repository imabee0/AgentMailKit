-- Divergence 1 (P1 gate, `reference/fixtures/25-p1-gate-conformance.txt`): `GET /v0/organizations`
-- emitted 5 of the reference's 17 fields. `amk_types::pod::Organization` already carries all of
-- them (frozen); this gives eight of the missing ten a column. `inbox_limit`/`domain_limit`
-- already had one (0001). `billing_plan_id`/`clerk_organization_id` are the other two missing
-- fields and are excluded by decision — no billing surface, no auth-vendor coupling — so they get
-- no column here and never will.
--
-- All eight nullable, no defaults: none of them is settable by any endpoint (`NewOrganization`
-- only ever sets `name`, at `amk init` time) — they are operator configuration, reachable today
-- only by a direct `UPDATE`. A default here would be a live outage waiting to happen: `0` on a
-- send-limit column means "send nothing", the opposite of "no configured limit", so absent must
-- stay `NULL` (and stay omitted on the wire — `amk_types::pod::Organization`'s own
-- `skip_serializing_if` already handles that half).
alter table organizations
    add column name text,
    add column daily_send_limit bigint,
    add column five_minute_send_limit bigint,
    add column first_day_recipient_limit bigint,
    add column first_week_recipient_limit bigint,
    add column tracking_allowed boolean,
    add column authentication_id text,
    add column authentication_type text;
