# Proposed `.claude/settings.json`

`settings.json` here is a drop-in replacement for `.claude/settings.json`. **The agent cannot apply
it**: the Claude Code classifier blocks an agent editing its own permissions, which is the layer
working exactly as designed — an agent that can widen its own grants has no grants. So it is staged
here for a human to review and move:

```bash
diff -u .claude/settings.json docs/proposed/settings.json   # read it first
cp docs/proposed/settings.json .claude/settings.json
```

## What changes, and why

**+68 allow entries.** Every one was used during real work in this repository and produced an
approval prompt mid-run. `CLAUDE.md`'s own rule is that an approval prompt is *a defect in this
list, not friction* — so the list is what gets fixed. Each prompt is also a stop, which is the
thing this whole change set exists to remove: an unattended session that hits one is finished until
someone comes back to it.

**−4 removed.**

| Removed | Why |
|---|---|
| `Bash(docker exec amk-dev-postgres:*)` | `dev-db.sh` drives `initdb`/`pg_ctl` directly; no container exists to exec into |
| `Bash(docker ps:*)` | same |
| `Bash(git -C /home/imma/projects/AgentMailKit push origin main:*)` | a hardcoded workstation path, meaningless anywhere else |
| `Bash(python3:*)` | replaced by four narrower forms — see below |

**The `python3` narrowing is the security half.** Unrestricted `python3` routed around every other
rule in the file. `python3 -c "import subprocess; subprocess.run(['git','reset','--hard'])"` matches
no git pattern; `urllib.request.urlopen(...)` matches no curl pattern. The fine-grained
restrictions were only ever enforced against *direct shell invocations*, and anything willing to
type ten more characters was unconstrained. It is now:

```
Bash(python3 conformance/:*)   Bash(python3 scripts/:*)
Bash(python3 -m venv:*)        Bash(python3 -c:*)
```

`python3 -c` is deliberately kept — the gate scripts use it for JSON extraction throughout, and
removing it would break `p1-gate.sh`, `binary-smoke.sh` and `affected-crates.sh`. It is a real
residual: `-c` can still express anything. Narrowing it further means rewriting those call sites to
use `jq`, which is worth doing and is not in this change.

**The deny list is UNCHANGED**, deliberately. It is correct as written: no secret-printing
(`sdxd get`, `gh auth token`), no force-push, no history rewriting, no `sudo`. `git reset --hard`
stays denied and earned it twice during this session — it destroyed uncommitted work during
mutation testing on two separate occasions, and the second time the deny list is what stopped it.
