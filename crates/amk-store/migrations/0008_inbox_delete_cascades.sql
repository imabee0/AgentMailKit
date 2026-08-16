-- Fixture 22 (`reference/fixtures/22-org-mount-and-delete-semantics.txt`) observed
-- `DELETE /v0/inboxes/{inbox_id}` return 202 unconditionally, with no emptiness precondition of
-- any kind. Under 0003-0007's declared FK behaviour (the default, NO ACTION), an inbox that has
-- ever received a message or been given a scoped api key was permanently undeletable — that
-- contradicts the unconditional 202 and makes the main path unreachable. Cascade the three FKs
-- referencing `inboxes`, plus `messages_thread_id_fkey`: the inbox cascade deletes `threads` rows
-- whose `messages` are being deleted by a *different* cascade in the same statement, and
-- `messages_thread_id_fkey` has to be ON DELETE CASCADE too or that second cascade trips it.
--
-- Deliberately does NOT touch any FK referencing `pods` (`inboxes_pod_id_fkey`,
-- `threads_pod_id_fkey`, `messages_pod_id_fkey`, `api_keys_pod_id_fkey`): fixture 22 also observed
-- `DELETE /v0/pods/{pod_id}` on a pod that still owns an inbox return 409 `cannot_delete` — a
-- pod delete must still trip `inboxes_pod_id_fkey` at the database, which `pods::delete`
-- (`StoreError::PodNotEmpty`) turns into that 409. `inboxes::delete` and `pods::delete` are
-- deliberately opposite answers to the same question, not an inconsistency.
--
-- [TESTED] against the dev database, 2026-08-16, inside a rolled-back transaction, with one org /
-- pod / inbox / thread / message / inbox-scoped key seeded behind it:
--   delete from pods ...     ->  ERROR 23503, constraint "inboxes_pod_id_fkey"  <- refusal survives
--   delete from inboxes ...  ->  DELETE 1, and afterwards:
--                                inboxes 0 | threads 0 | messages 0 | api_keys 0 | pods 1
-- No ordering problem: both cascades run in the same statement without `messages_thread_id_fkey`
-- firing on rows that are themselves being deleted.
alter table threads  drop constraint threads_inbox_id_fkey,
  add constraint threads_inbox_id_fkey   foreign key (inbox_id)  references inboxes (inbox_id)  on delete cascade;
alter table messages drop constraint messages_inbox_id_fkey,
  add constraint messages_inbox_id_fkey  foreign key (inbox_id)  references inboxes (inbox_id)  on delete cascade;
alter table api_keys drop constraint api_keys_inbox_id_fkey,
  add constraint api_keys_inbox_id_fkey  foreign key (inbox_id)  references inboxes (inbox_id)  on delete cascade;
alter table messages drop constraint messages_thread_id_fkey,
  add constraint messages_thread_id_fkey foreign key (thread_id) references threads (thread_id) on delete cascade;
