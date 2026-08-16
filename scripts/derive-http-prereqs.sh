#!/usr/bin/env bash
# Scope derivation for .claude/contracts/amk-store-http-prereqs.md.
#
# The contract pastes this script's output and treats it as the scope. A reviewer re-runs the
# script rather than reading the list — a recalled scope is what cost the id-safety dispatch four
# correction rounds, and every one of those five missing sites was found by enumerating.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

echo "== 1. every amk-store list function, with its return type =="
python3 - <<'PY'
import pathlib, re
for f in sorted(pathlib.Path('crates/amk-store/src').glob('*.rs')):
    src = f.read_text()
    for m in re.finditer(r'^pub async fn list\((.*?)\) -> (.*?) \{', src, re.S | re.M):
        raw = [a.strip() for a in m.group(1).replace('\n', ' ').split(',') if a.strip()]
        # The pool argument is identical everywhere and only adds noise to the comparison.
        args = ', '.join(['pool'] + raw[1:])
        print(f"{f.name}: pub async fn list({args})")
        print(f"{' ' * len(f.name)}  -> {m.group(2).strip()}")
PY

echo
echo "== 2. paginated GETs among the 25 first-dispatch operations =="
python3 - <<'PY'
import json, re
spec = json.load(open('reference/openapi.json'))
ops = [l.strip().strip('|').split('|') for l in open('.claude/contracts/amk-http.md')
       if re.match(r'^\| *`(GET|POST|PATCH|DELETE)` *\| *`/v0/', l)]
ops = [(a.strip().strip('`'), b.strip().strip('`')) for a, b in ops]
paged = [(m, p) for m, p in ops
         if m == 'GET' and any(q.get('name') == 'page_token'
                               for q in spec['paths'][p][m.lower()].get('parameters', []))]
print(f"{len(ops)} operations in the dispatch table; {len(paged)} carry page_token:")
for m, p in paged:
    print(f"  {m:6} {p}")
PY

echo
echo "== 3. foreign keys that make a DELETE fail with SQLSTATE 23503 =="
python3 - <<'PY'
import pathlib, re
rows = []
for f in sorted(pathlib.Path('crates/amk-store/migrations').glob('*.sql')):
    table = None
    for line in f.read_text().splitlines():
        m = re.search(r'CREATE TABLE (?:IF NOT EXISTS )?(\w+)', line)
        if m:
            table = m.group(1)
        m = re.search(r'(\w+)\s+\w+.*REFERENCES (\w+)', line)
        if m and table:
            rows.append((f"{table}.{m.group(1)}", m.group(2)))
w = max(len(a) for a, _ in rows)
for a, b in rows:
    print(f"  {a:<{w}} -> {b}")
PY

echo
echo "== 4. every catch of a database-error class in amk-store =="
grep -rn "sqlx::Error::Database\|is_unique_violation\|is_foreign_key_violation\|\.constraint()" \
  crates/amk-store/src/*.rs | sed 's|crates/amk-store/src/|  |; s/ *$//'

echo
echo "== 5. the minted-key constants, against fixture 23 =="
grep -n "const PREFIX_TAG\|const SECRET_LEN\|const VISIBLE_LEN" crates/amk-store/src/api_keys.rs \
  | sed 's|^|  api_keys.rs:|'
grep -h '^  prefix \|^  api_key ' reference/fixtures/23-inbox-defaults-and-key-shape.txt \
  | sed 's/^ */  fixture 23:  /'
