# Install the pb-mapper Suite Skill

Install this Skill without cloning the pb-mapper repository.

1. If the current harness supports installing a Skill directly from a remote
   URL, use that native mechanism with the `SKILL.md` URL below.
2. Otherwise, detect the harness's native **user-level** skills directory. Use
   its existing convention rather than installing into the current project;
   for example, Codex normally uses `${CODEX_HOME:-$HOME/.codex}/skills`, Claude
   Code uses `$HOME/.claude/skills`, and Agent Skills compatible harnesses may
   use `$HOME/.agents/skills`.
3. Create a `pb-mapper-suite` directory there and download
   `https://raw.githubusercontent.com/acking-you/pb-mapper/master/skills/pb-mapper-suite/SKILL.md`
   as `pb-mapper-suite/SKILL.md`.
4. Verify the downloaded file starts with YAML frontmatter whose name is
   `pb-mapper-suite`. No other repository file is required at runtime.
5. Tell the user where the Skill was installed and whether the current harness
   needs a restart or new session before it will be discovered.

When updating an existing installation, replace only that Skill's `SKILL.md`
after a successful download; preserve unrelated skills and configuration.
