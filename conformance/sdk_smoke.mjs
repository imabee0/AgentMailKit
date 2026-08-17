// P1 gate: the UNMODIFIED official Node SDK driving our server.
//
// The Python smoke proves one official client works. This proves the OTHER one does — they are
// generated from the same spec but not from the same code, and the plan's gate says "Python+Node"
// for that reason.
//
// What is actually different here, probed against the installed 0.5.19 rather than assumed:
//   * it maps the wire's snake_case onto camelCase, so a field we misspell or omit arrives as
//     `undefined` rather than raising — the assertions below are what turn that into a failure;
//   * it coerces RFC 3339 strings into real `Date` objects, which is a second, independent parser
//     for the timestamp format `Timestamp` is wire-exact about;
//   * it routes an `am_eu_`-prefixed key to `api.agentmail.eu`, the one client behaviour that
//     would take a caller off our base URL entirely [SPEC:sdk environments.ts, Client.ts:80].
// What it does NOT do — the obvious guess, and wrong: reject a response missing a required field.
// Every resource client parses with `skipValidation: true, unrecognizedObjectKeys: "passthrough"`,
// so a missing field is accepted as `undefined` and our extra live fields pass through un-mapped.
//
// Nothing here is AgentMailKit-aware: only the base URL changes.
//
// Usage:  AMK_BASE=http://127.0.0.1:8111 AMK_KEY=<root key> node sdk_smoke.mjs

import { AgentMailClient } from "agentmail";

let checks = 0;
const failures = [];

function check(label, got, want) {
  checks++;
  const ok = JSON.stringify(got) === JSON.stringify(want);
  console.log(`  [${ok ? "PASS" : "FAIL"}] ${label}` +
    (ok ? "" : `\n         expected ${JSON.stringify(want)}, got ${JSON.stringify(got)}`));
  if (!ok) failures.push(label);
}

// A timestamp the SDK could not parse is passed through as the raw string (skipValidation), so
// `instanceof Date` is exactly the assertion that separates "RFC 3339 with three fractional digits
// and a Z" from anything else our server might emit.
const isDate = (v) => v instanceof Date && !Number.isNaN(v.getTime());

const base = process.env.AMK_BASE;
const client = new AgentMailClient({
  apiKey: process.env.AMK_KEY,
  environment: { http: base, websockets: base.replace("http", "ws") },
});

const tag = Math.random().toString(16).slice(2, 10);
console.log(`official agentmail Node SDK -> ${base}\n`);

// ---- identity ---------------------------------------------------------------------------------
console.log("auth.me / organizations.get");
const me = await client.auth.me();
check("auth.me returns an identity with an organization_id", typeof me.organizationId, "string");
check("scope_id == organization_id for an org-scoped key", me.scopeId, me.organizationId);

const org = await client.organizations.get();
check("organizations.get agrees with the identity", org.organizationId, me.organizationId);
// The Node SDK maps snake_case to camelCase, so this also proves our field NAMES are right —
// a misspelled wire field would arrive as undefined rather than as a mapping error.
check("inbox_count survives the SDK's camelCase mapping", typeof org.inboxCount, "number");
check("the organization's timestamps parsed into Date",
  [isDate(org.createdAt), isDate(org.updatedAt)], [true, true]);

// ---- pods -------------------------------------------------------------------------------------
console.log("\npods: create -> list -> get -> delete");
const pod = await client.pods.create({ name: `node smoke ${tag}`, clientId: `node-pod-${tag}` });
check("pods.create round-trips client_id through camelCase", pod.clientId, `node-pod-${tag}`);

const replay = await client.pods.create({ name: "ignored", clientId: `node-pod-${tag}` });
check("client_id replay is idempotent", replay.podId, pod.podId);

const pods = await client.pods.list({ limit: 100 });
check("pods.list includes it", pods.pods.some((p) => p.podId === pod.podId), true);

const gotPod = await client.pods.get(pod.podId);
check("pods.get round-trips the id", gotPod.podId, pod.podId);
check("the pod's timestamps parsed into Date",
  [isDate(gotPod.createdAt), isDate(gotPod.updatedAt)], [true, true]);

// ---- inboxes ----------------------------------------------------------------------------------
console.log("\ninboxes: create -> get -> update -> list -> delete");
const inbox = await client.inboxes.create({ username: `nodesmoke${tag}` });
check("inbox_id IS the email address", inbox.inboxId, inbox.email);

const gotInbox = await client.inboxes.get(inbox.inboxId);
check("inboxes.get round-trips the id", gotInbox.inboxId, inbox.inboxId);
check("the inbox's timestamps parsed into Date",
  [isDate(gotInbox.createdAt), isDate(gotInbox.updatedAt)], [true, true]);
check("inboxes.get resolves a differently-cased id",
  (await client.inboxes.get(inbox.inboxId.toUpperCase())).inboxId, inbox.inboxId);

const updated = await client.inboxes.update(inbox.inboxId, { displayName: `Node ${tag}` });
check("inboxes.update sets display_name", updated.displayName, `Node ${tag}`);

const inboxes = await client.inboxes.list({ limit: 100 });
check("inboxes.list includes it", inboxes.inboxes.some((i) => i.inboxId === inbox.inboxId), true);

// ---- api keys ---------------------------------------------------------------------------------
console.log("\napi-keys: create -> list -> authenticate -> delete");
const key = await client.apiKeys.create({ name: `node smoke key ${tag}` });
check("api_keys.create returns the plaintext once", typeof key.apiKey, "string");
check("the returned prefix is the key's first characters", key.apiKey.startsWith(key.prefix), true);
// The node SDK routes an am_eu_ key to api.agentmail.eu, which would leave our base URL entirely.
check("a minted key never begins am_eu_", key.apiKey.startsWith("am_eu_"), false);

const minted = new AgentMailClient({
  apiKey: key.apiKey,
  environment: { http: base, websockets: base.replace("http", "ws") },
});
check("the minted key authenticates", (await minted.auth.me()).organizationId, me.organizationId);

// ---- pagination -------------------------------------------------------------------------------
console.log("\npagination: walk a page boundary with the SDK's own token");
const page1 = await client.inboxes.list({ limit: 1 });
check("a bounded page returns one item", page1.inboxes.length, 1);
if (page1.nextPageToken) {
  const page2 = await client.inboxes.list({ limit: 1, pageToken: page1.nextPageToken });
  check("the token advances to a different inbox",
    page2.inboxes[0].inboxId !== page1.inboxes[0].inboxId, true);
} else {
  check("a next_page_token was expected", "absent", "present");
}

// ---- delete -----------------------------------------------------------------------------------
console.log("\ndelete: the other half of the CRUD cycle");
await client.apiKeys.delete(key.apiKeyId);
check("the deleted key is gone",
  (await client.apiKeys.list({ limit: 100 })).apiKeys.some((k) => k.apiKeyId === key.apiKeyId), false);

await client.inboxes.delete(inbox.inboxId);
check("the deleted inbox is gone",
  (await client.inboxes.list({ limit: 100 })).inboxes.some((i) => i.inboxId === inbox.inboxId), false);

await client.pods.delete(pod.podId);
check("the deleted pod is gone",
  (await client.pods.list({ limit: 100 })).pods.some((p) => p.podId === pod.podId), false);

console.log(`\n${checks} checks, ${failures.length} failed`);
for (const f of failures) console.log(`  FAILED: ${f}`);
process.exit(failures.length ? 1 : 0);
