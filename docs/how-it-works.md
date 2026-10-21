# How things get linked

**Project.** An agent belongs to the project when its cwd is in a repo with the same git common directory, so worktrees outside the repo folder count.

**Agent → issue**, from the agent's branch (never the default branch), first match wins:
1. An issue number in the branch name: `fix-score-floor-104`, `user/42-add-thing`.
2. A branch name that starts an open issue's title in branch form: `prototype-delegating-search-get-to-a-codex-subag` → #107.
3. An open PR from that branch that closes an issue.
4. One explicit `#number` in the agent's task or pane title, when it names an open issue. This also works when agents share the default branch. Titles naming multiple open issues are left unlinked.

With no match, the agent is listed under "No issue".

**Issue → module.** [Jev](https://openrouter.ai/docs/guides/community/jev) (`typesafe/jev-1.13`) reads the issue's title and body and answers one yes/no question per module, built from its `label` and `description` (or `paths` when there is no description). The issue goes under its most likely module when that probability reaches `min_confidence`, shown after the title. Otherwise it goes under "Not placed in a module" with its best guess. No labels or diffs are read.

Only live agents and the issues they are on are shown. Sub-issues are grouped under their parent.
