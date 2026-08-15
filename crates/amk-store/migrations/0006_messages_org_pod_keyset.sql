-- Supporting indexes for messages::list's org- and pod-mount keyset walk.
--
-- 0005's `messages_keyset_idx (inbox_id, "timestamp", message_id)` only serves the inbox mount,
-- where `inbox_id` is pinned by equality and the index's remaining columns satisfy the
-- `("timestamp", inbox_id, message_id)` ordering the query now always uses (see messages.rs's
-- `list` doc comment for why `inbox_id` joined the tiebreak — a Message-ID is only guaranteed
-- unique within one inbox, so two different inboxes can share one at the org/pod mounts, where
-- `inbox_id` is unpinned). At the org and pod mounts, `inbox_id` is not fixed by the WHERE clause,
-- so a leading-`inbox_id` index cannot drive an ordered range scan over `("timestamp", inbox_id,
-- message_id)`; these two indexes give the planner a matching leading column (organization_id /
-- pod_id) followed by the exact keyset order.
CREATE INDEX messages_org_keyset_idx ON messages (organization_id, "timestamp", inbox_id, message_id);
CREATE INDEX messages_pod_keyset_idx ON messages (pod_id, "timestamp", inbox_id, message_id);
