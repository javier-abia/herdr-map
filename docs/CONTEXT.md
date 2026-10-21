# herdr-map

A terminal view of which GitHub issue each herdr agent is working on, and which modules of the project those issues touch.

## Language

**Module**:
A stable part of the project (MCP server, Search, Ingest…), declared in the module map as an id, a label, a short description and path globs (tests included). The description is what the classifier reads.
_Avoid_: feature (the v0 name; "feature" now means what an issue builds)

**Module map**:
The `.herdr-map.toml` file: `[project]` (name, root, `min_confidence`), `[[module]]` entries, and `[[edge]]` dependencies between modules.
_Avoid_: feature map

**Issue**:
An open GitHub issue that a live agent is working on: the feature or fix being built. Issues with no agent are not shown. Sub-issues are grouped under their parent.
_Avoid_: task, ticket

**Agent**:
A coding agent running in a herdr pane, with a status (working / idle / blocked), a cwd and a title.
_Avoid_: worker, session

**World**:
The facts collected in one tick: module map, agents, per-agent branches, GitHub issues and PRs, and classifications. All I/O happens in collectors that build it; the graph is a pure function of it.
_Avoid_: state, snapshot (a herdr snapshot is only one input)

**Graph**:
Modules → issues → agents, plus unplaced issues, agents with no issue, and elsewhere agents.

**Link**:
An edge with a target, a source and a confidence. Sources: `branch-number`, `title-match`, `pr` (confidence 1.0) and `classifier` (the classifier's probability).

**Agent → issue link**:
From the agent's branch (never the default branch), first match wins: issue number in the branch name, then branch name as the start of an open issue's title in branch form, then an open PR from that branch closing an issue. If the branch gives no match, one explicit `#number` in the agent task or pane title links to an open issue, including on the default branch. Multiple different open issue numbers are ambiguous and leave the agent unlinked.

**Issue → module link**:
Decided by the classifier alone, from the issue's title and body and the module map; GitHub labels and diffs are never used. The module with the highest probability places the issue if it reaches `min_confidence`. One module per issue.

**Classifier**:
Jev (`typesafe/jev-1.13`, TypeSafe's structured decision model, called through OpenRouter). It answers one yes/no question per module and returns a calibrated probability for each. Results are cached on disk until the issue, a module's id, label or description, the set of modules (one added or removed) or the model changes. Editing a module's path globs does not invalidate them.
_Avoid_: LLM (Jev returns no text)

**Unplaced**:
An issue whose best module is below `min_confidence`, or that has no classification yet (for example, offline with nothing cached). Shown with its best guess when there is one.

**Elsewhere**:
An agent whose cwd is outside the project's git common directory. Worktrees outside the repo folder that share it are in the project.
