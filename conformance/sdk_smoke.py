#!/usr/bin/env python3
"""P1 gate, second half: the UNMODIFIED official Python SDK driving our server.

The conformance diff proves our responses have the reference's *shape*. This proves the official
client can actually *use* them — deserialize into its own typed models, page, and round-trip a
create/list/get/update/delete cycle. A response can be structurally identical and still break a
client (a pydantic validator, a required field the model insists on), so neither check subsumes
the other.

Nothing here is AgentMailKit-aware: only the base URL changes, which is the entire V1 acceptance
criterion. Any import from `agentmail` is the real published package pinned in
`conformance/requirements-gate.txt`.

Usage:  AMK_BASE=http://127.0.0.1:8111 AMK_KEY=<root key> python3 sdk_smoke.py
"""
import os
import sys
import uuid

from agentmail import AgentMail, AgentMailEnvironment

failures = []
checks = 0


def check(label, got, want=None, predicate=None):
    """Assert and report in one line so the transcript IS the evidence."""
    global checks
    checks += 1
    ok = predicate(got) if predicate else (got == want)
    print(f"  [{'PASS' if ok else 'FAIL'}] {label}"
          + ("" if ok else f"\n         expected {want!r}, got {got!r}"))
    if not ok:
        failures.append(label)
    return ok


def main():
    base = os.environ["AMK_BASE"]
    client = AgentMail(
        environment=AgentMailEnvironment(http=base, websockets=base.replace("http", "ws")),
        api_key=os.environ["AMK_KEY"],
    )
    tag = uuid.uuid4().hex[:8]
    print(f"official agentmail SDK -> {base}\n")

    # ---- identity -----------------------------------------------------------------------------
    print("auth.me / organizations.get")
    me = client.auth.me()
    check("auth.me returns an Identity", type(me).__name__, "Identity")
    check("scope_id == organization_id for an org-scoped key", me.scope_id, me.organization_id)
    org = client.organizations.get()
    check("organizations.get returns an Organization", type(org).__name__, "Organization")
    check("organization_id agrees with the identity", org.organization_id, me.organization_id)
    # The field the P1 gate added: the SDK's own model must accept it.
    check("the SDK model exposes inbox_count", hasattr(org, "inbox_count"), True)

    # ---- pods: create / list / get / delete ----------------------------------------------------
    print("\npods: create -> list -> get -> delete")
    pod = client.pods.create(name=f"smoke pod {tag}", client_id=f"smoke-pod-{tag}")
    check("pods.create returns a Pod", type(pod).__name__, "Pod")
    check("the pod carries the client_id we sent", pod.client_id, f"smoke-pod-{tag}")

    replay = client.pods.create(name="ignored", client_id=f"smoke-pod-{tag}")
    check("client_id replay is idempotent (same pod_id, no duplicate)", replay.pod_id, pod.pod_id)

    listed = client.pods.list(limit=100)
    check("pods.list includes it", any(p.pod_id == pod.pod_id for p in listed.pods), True)
    check("pods.list reports a count", isinstance(listed.count, int), True)

    fetched = client.pods.get(pod_id=pod.pod_id)
    check("pods.get round-trips the id", fetched.pod_id, pod.pod_id)

    # ---- inboxes: create / list / get / update / delete -----------------------------------------
    print("\ninboxes: create -> list -> get -> update -> delete")
    # Namespaced under `agentmail.inboxes`, not re-exported at the package root — the root only
    # carries `CreateDraftRequest`, which is a different resource. Import it where it lives.
    from agentmail.inboxes import CreateInboxRequest

    inbox = client.inboxes.create(request=CreateInboxRequest(username=f"smoke{tag}"))
    check("inboxes.create returns an Inbox", type(inbox).__name__, "Inbox")
    check("inbox_id IS the email address", inbox.inbox_id, inbox.email)

    got = client.inboxes.get(inbox_id=inbox.inbox_id)
    check("inboxes.get round-trips the id", got.inbox_id, inbox.inbox_id)
    # inbox_id folds case (fixture 18) and travels in a path segment containing '@'.
    upper = client.inboxes.get(inbox_id=inbox.inbox_id.upper())
    check("inboxes.get resolves a differently-cased id", upper.inbox_id, inbox.inbox_id)

    updated = client.inboxes.update(inbox_id=inbox.inbox_id, display_name=f"Smoke {tag}")
    check("inboxes.update sets display_name", updated.display_name, f"Smoke {tag}")

    inbox_list = client.inboxes.list(limit=100)
    check("inboxes.list includes it", any(i.inbox_id == inbox.inbox_id for i in inbox_list.inboxes), True)

    # ---- api keys: create / list / delete -------------------------------------------------------
    print("\napi-keys: create -> list -> delete")
    key = client.api_keys.create(name=f"smoke key {tag}")
    check("api_keys.create returns the plaintext once", bool(key.api_key), True)
    check("the returned prefix is the key's first characters",
          key.api_key.startswith(key.prefix), True)
    check("a minted key never begins am_eu_ (it would leave our base URL)",
          key.api_key.startswith("am_eu_"), False)

    keys = client.api_keys.list(limit=100)
    check("api_keys.list includes it", any(k.api_key_id == key.api_key_id for k in keys.api_keys), True)

    # The minted key must actually authenticate — the whole point of minting one.
    second = AgentMail(
        environment=AgentMailEnvironment(http=base, websockets=base.replace("http", "ws")),
        api_key=key.api_key,
    )
    check("the minted key authenticates", second.auth.me().organization_id, me.organization_id)

    # ---- pagination -----------------------------------------------------------------------------
    print("\npagination: walk a page boundary with the SDK's own token")
    first = client.inboxes.list(limit=1)
    check("a bounded page returns one item", len(first.inboxes), 1)
    if first.next_page_token:
        second_page = client.inboxes.list(limit=1, page_token=first.next_page_token)
        check("the SDK's page_token advances to a different inbox",
              second_page.inboxes[0].inbox_id != first.inboxes[0].inbox_id, True)
    else:
        check("more than one inbox exists so a token was expected", "no next_page_token", None)

    # ---- teardown, which is itself the delete half of the cycle ---------------------------------
    print("\ndelete: the other half of the CRUD cycle")
    client.api_keys.delete(api_key_id=key.api_key_id)
    check("the deleted key is gone from the listing",
          any(k.api_key_id == key.api_key_id for k in client.api_keys.list(limit=100).api_keys),
          False)

    client.inboxes.delete(inbox_id=inbox.inbox_id)
    check("the deleted inbox is gone from the listing",
          any(i.inbox_id == inbox.inbox_id for i in client.inboxes.list(limit=100).inboxes),
          False)

    client.pods.delete(pod_id=pod.pod_id)
    check("the deleted pod is gone from the listing",
          any(p.pod_id == pod.pod_id for p in client.pods.list(limit=100).pods),
          False)

    print(f"\n{checks} checks, {len(failures)} failed")
    for f in failures:
        print(f"  FAILED: {f}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
