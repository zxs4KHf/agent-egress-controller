# Issue tracker: GitHub Issues

Issues and specs for this repo live in GitHub Issues on `zxs4KHf/agent-egress-controller`. Use the `gh` CLI.

## Conventions

- One issue per ticket. A larger feature gets a parent issue that links its child issues in a task list.
- Durable specs live in `docs/` (for v1: `docs/SPEC.md`); an issue links to the section it implements instead of copying it.
- Triage state is a label (see `triage-labels.md`).
- Discussion and evidence go in issue comments. Evidence is marked **[已验证]** (command and output) or **[推断]**.
- Never paste credentials, exit IPs you do not want public, or unredacted logs. The tray's「复制排查提示词」output is already redacted.

## When a skill says "publish to the issue tracker"

`gh issue create --repo zxs4KHf/agent-egress-controller --title "..." --body-file <file> --label needs-triage`

## When a skill says "fetch the relevant ticket"

`gh issue view <number> --repo zxs4KHf/agent-egress-controller --comments`

## Wayfinding operations

- **Map**: a parent issue whose body holds Notes / Decisions-so-far / Fog and a task list of children.
- **Child ticket**: an issue linked from the map, with a `type:` label (`research`/`prototype`/`grilling`/`task`).
- **Blocking**: a `Blocked by: #N, #N` line in the body. A ticket is unblocked when every listed issue is closed.
- **Frontier**: open, unblocked, unassigned children of the map; lowest number first.
- **Claim**: assign yourself before any work.
- **Resolve**: comment the answer under an `## Answer` heading, close the issue, then add a one-line pointer to the map's Decisions-so-far.

Maintainers may also keep private notes in `.scratch/` locally; that directory is gitignored and never part of the public record.
