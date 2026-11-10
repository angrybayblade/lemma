# Agent skills

Skills that teach coding agents to use Lemma. Each directory is one skill in the format Claude Code reads (`SKILL.md` with a name and description, plus reference files it loads when needed).

| Skill | Use when |
| --- | --- |
| [`lemma-mode`](lemma-mode/SKILL.md) | `/lemma-mode`: the workflow for a change other code depends on, sized to the change, ending in proof. See [Lemma mode](../docs/lemma-mode.md) |
| [`lemma-impact`](lemma-impact/SKILL.md) | Mapping every use and every caller, across repositories, before changing an operation, schema or shared symbol |
| [`lemma-verify`](lemma-verify/SKILL.md) | Proving a change with `lemma check` at the end of each unit, fixing what it reports, and an opt-in Stop hook that checks before the agent finishes |
| [`lemma-behavior`](lemma-behavior/SKILL.md) | Proving a runtime claim by running the real handler, kept as a behavior (beta) |
| [`lemma-usage`](lemma-usage/SKILL.md) | Navigating code, finding references or impact, and reading `lemma check` findings |
| [`lemma-onboarding`](lemma-onboarding/SKILL.md) | Setting Lemma up for a set of repositories: install, index, write `mod.lm` files, CI |
| [`lemma-grammar`](lemma-grammar/SKILL.md) | Writing or changing requirements: relations, rules, `require`, `check main`, `allow` |
| [`lemma-audit`](lemma-audit/SKILL.md) | Auditing a system's `.lm` description against the code and closing the gaps |

Install with the binary (`LEMMA_SKILLS=1`, see [Onboarding](../docs/onboarding.md#agent-skills)), or copy the directories into `~/.claude/skills/` or a repository's `.claude/skills/`.
