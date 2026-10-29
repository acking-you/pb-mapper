# Claude Code Review

`.github/workflows/claude-code-review.yml` runs one automatic review when a
same-repository pull request is opened, including a draft PR. Pushes, reopening
the PR, and switching between draft and ready do not trigger another review.
Fork PRs and Dependabot PRs remain excluded. An explicit **Re-run jobs** action
can repeat an existing run; this is a manual retry, not another automatic run.

The review can read the complete diff, related source and tests, and run checks.
There is no configured turn, tool-call, file-count, diff-size, output-length,
finding-count, or spending cap. The job has one 30-minute timeout, including
setup. Normal model context and provider limits still apply. Confirmed findings
are posted inline, and the action retains its report and structured output.

Configure repository secrets in **Settings → Secrets and variables → Actions →
Secrets**:

| Secret | Purpose |
| --- | --- |
| `ANTHROPIC_BASE_URL` | Required Anthropic-compatible gateway URL. |
| `ANTHROPIC_API_KEY` | Required gateway credential. |
| `ANTHROPIC_CUSTOM_HEADERS` | Optional gateway headers. |
| `CLAUDE_REVIEW_MODEL` | Model used only by this review action. Defaults to `claude-opus-5-5` when unset or empty. |

Update `CLAUDE_REVIEW_MODEL` to change the model for subsequent runs without
editing the workflow. The former `CLAUDE_MODEL` repository variable is no longer
used. Removing `--max-turns` leaves turns unlimited, as documented in the
[Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference).

This workflow is advisory. Missing gateway secrets produce warnings and skip
the review. Both the review step and job allow failure, and the final status
step reports incomplete reviews without failing CI. Findings never become a
failing quality gate. Keep this optional workflow out of branch-protection
required checks: it deliberately does not produce a new check on every push,
and GitHub may still display infrastructure failures, cancellation, or timeout
status even when a job allows failure.

Workflow changes take effect after merging into the default branch. The pinned
Claude action's existing GitHub App/OIDC workflow validation remains enabled;
this change does not rerun reviews on already-open PRs.
