# Agent skills

Skills that teach coding agents to use Lemma. Each directory is one skill in the format Claude Code reads (`SKILL.md` with a name and description, plus reference files it loads when needed).

| Skill | Use when |
| --- | --- |
| [`lemma-usage`](lemma-usage/SKILL.md) | Navigating code, finding references or impact, and reading `lemma check` findings |
| [`lemma-onboarding`](lemma-onboarding/SKILL.md) | Setting Lemma up for a set of repositories: install, index, write `mod.lm` files, CI |
| [`lemma-grammar`](lemma-grammar/SKILL.md) | Writing or changing requirements: relations, rules, `require`, `check main`, `allow` |
| [`lemma-audit`](lemma-audit/SKILL.md) | Auditing a system's `.lm` description against the code and closing the gaps |

Install with the binary (`LEMMA_SKILLS=1`, see [Onboarding](../docs/onboarding.md#agent-skills)), or copy the directories into `~/.claude/skills/` or a repository's `.claude/skills/`.
