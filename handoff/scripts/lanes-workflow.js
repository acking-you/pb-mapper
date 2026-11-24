export const meta = {
  name: 'pc-optimization-lanes',
  description: 'Implement planned PRs in parallel lanes (each PR on the previous one in its lane), review each from two angles, then fix confirmed findings',
  phases: [
    { title: 'Implement', detail: 'one agent per PR on its own branch' },
    { title: 'Review', detail: 'correctness and effect reviewers per PR' },
    { title: 'Fix', detail: 'verify each finding and fix the real ones on the PR branch' },
  ],
}

const S = '/private/tmp/claude-501/-Users-lb7666--claude-spares-spare-vPIbq7/2de9709e-ea4b-442d-a692-7f9e8fd50cdd/scratchpad'
const AH = S + '/airhand-latest'
const BASE = args.base
const DATE = args.date
const BRIEFS = args.briefs
const LANES = args.lanes
const BASE_NOTE = args.base_note || ''
if (!Array.isArray(LANES) || LANES.length === 0 || !LANES.every(l => Array.isArray(l) && l.length > 0)) {
  throw new Error('args.lanes must be a non-empty array of non-empty arrays of {name, title}')
}

const COMMON = `
You work on pb-mapper (a Rust tunnel and remote desktop with a Flutter app). This task is part of a reviewed plan to give every PC platform pb-mapper supports (Windows host and viewer, macOS host and viewer, Linux viewer) the best performance the reference project airhand inspires. airhand's source is at ${AH}. The plan's research and independent critiques, by item id, are in ${S}/pc-research.json (keys: areas[].research.candidates[], areas[].critique.reviews[] and .missed[]); search it for your items when the brief leaves a detail open.

${BASE_NOTE}

Machine: a 12-core Apple M6 Mac with 24 GiB, macOS 27.0 (26A425), Rust 1.99.1. Other agents build other branches in parallel worktrees on this machine: never touch another worktree, never change global git, cargo or Flutter config, never push, never open a PR, and never run commands that delete outside your own worktree. If a Flutter command waits on the SDK's startup lock, wait. Before you return, delete your own worktree's build output (rm -rf target ui/build in your worktree) to keep the disk free.

Rules: CLAUDE.md is injected; follow it exactly (compatibility invariants, lints, errors, tests, performance, verification). Read docs/rust-engineering-guide.md before writing or changing Rust. Desktop OS APIs live only under crates/pb-mapper-desktop/src/platform/<os>/. Remote desktop messages reject unknown fields: anything new on the wire is sent only to a peer that listed the matching capability; ALPN stays pb-mapper/desktop/2. HELPER_PROTOCOL changes together with both .v<n> pipe names (bump it to one above the value on your base). Zero cost by default; no per-packet copies, queues or spawns on a data path.

Local tool notes: Flutter is at ~/Developer/flutter/bin (3.50.1; CI pins 3.44.9). On it, one app_test.dart case ("the title bar never overflows while this computer is viewed") fails and dart format reports about 20 unrelated files: SDK drift, not a regression; check formatting only on the files you changed (dart format --output=none --set-exit-if-changed <files>). Running Flutter here rewrites ui/pubspec.lock, ui/analysis_options.yaml, ui/macos/Flutter/*.xcconfig and creates ui/macos/Podfile: restore them (git checkout -- ui/pubspec.lock ui/analysis_options.yaml ui/macos/Flutter; rm -f ui/macos/Podfile) before every commit. bun is not installed. python3 is 3.9. Windows code (Rust under platform/windows, the helper, ui/windows C++) compiles only in CI on windows-latest: check every Windows API you call against the windows crate's source in ~/.cargo/registry/src (the version in Cargo.lock) and against Microsoft's documented semantics. The Linux runner (ui/linux) compiles only in CI (ubuntu-22.04, flutter build linux --debug): check GStreamer, GTK and Flutter Linux embedder APIs against their headers and the versions Ubuntu 22.04 ships (GStreamer 1.20, GTK 3.24). You cannot run Windows or Linux code; be twice as careful with it.

Measurements: numbers only from runs you actually made here, release builds, with the environment above; a recorded run goes under docs/benchmarks/${DATE}-<topic>/ (README.md plus raw output) and is indexed in docs/README.md. Never put a model estimate or an unmeasured figure in docs, CHANGELOG or comments. What needs a device is listed in your result, not claimed.

Commits: short imperative subject, a body that says why, ending with the line
Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
A user-visible change adds a bullet under "## [Unreleased]" at the top of CHANGELOG.md, in the style of the bullets already there. English and Chinese doc pairs change together.
`

const VERIF = { type: 'array', items: { type: 'object', properties: { command: { type: 'string' }, result: { type: 'string' } }, required: ['command', 'result'] } }

const IMPL = {
  type: 'object',
  properties: {
    branch: { type: 'string' },
    commits: { type: 'array', items: { type: 'string' }, description: 'sha and subject' },
    summary: { type: 'string', description: 'what was implemented, file by file' },
    deviations: { type: 'string', description: 'where and why the implementation differs from the brief' },
    verification: VERIF,
    measurements: { type: 'string', description: 'before/after numbers actually measured here, or "none"' },
    device_only: { type: 'string', description: 'what only CI or a device can verify' },
    complete: { type: 'boolean', description: 'false if any part of the brief is not done' },
  },
  required: ['branch', 'commits', 'summary', 'deviations', 'verification', 'measurements', 'device_only', 'complete'],
}

const REVIEW = {
  type: 'object',
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
          file: { type: 'string' },
          line: { type: 'integer' },
          issue: { type: 'string' },
          evidence: { type: 'string', description: 'the code, API documentation or test run that shows it' },
          fix: { type: 'string' },
        },
        required: ['severity', 'file', 'issue', 'evidence', 'fix'],
      },
    },
    verdict: { type: 'string', enum: ['ready', 'needs-fixes', 'rework'] },
    notes: { type: 'string' },
  },
  required: ['findings', 'verdict', 'notes'],
}

const FIX = {
  type: 'object',
  properties: {
    branch: { type: 'string' },
    commits: { type: 'array', items: { type: 'string' } },
    fixed: { type: 'array', items: { type: 'string' } },
    rejected: { type: 'array', items: { type: 'object', properties: { finding: { type: 'string' }, reason: { type: 'string' } }, required: ['finding', 'reason'] } },
    verification: VERIF,
    ready: { type: 'boolean' },
    remaining: { type: 'string' },
  },
  required: ['branch', 'commits', 'fixed', 'rejected', 'verification', 'ready', 'remaining'],
}

const branchOf = pr => 'perf2/' + pr.name

const brief = (pr, base) => `PR "${pr.title}" (name ${pr.name}, branch ${branchOf(pr)}, based on ${base}).
Its full brief (fields title, items, platforms, files, protocol_changes, design, verification, measurement, verifiable_here) is the entry named "${pr.name}" in ${BRIEFS}. Read it in full before anything else:
  python3 -I -c "import json; [print(json.dumps(p, indent=1)) for p in json.load(open('${BRIEFS}')) if p['name'] == '${pr.name}']"
The brief's DESIGN is what to build, VERIFICATION what to run, MEASUREMENT what to measure, and its files list is the plan's expectation, not a limit. Its file:line references were taken at 1515eda; your base ${base} has moved on (see the base note above), so find the code by name.`

const implPrompt = (pr, base) => `${COMMON}
Your task: implement this pull request completely.

${brief(pr, base)}

Start in your worktree with:
  git switch -c ${branchOf(pr)} ${base}
  git log -1 --oneline      (must match: git rev-parse ${base})
  git submodule update --init
Then read the code the design names before changing it. Where the design is wrong about the code, or an earlier PR on your base already did part of it, do what achieves its intent and record the deviation. Implement everything, with the tests the design lists (tests that fail without the change). Run every verification command that can run here, and fix what fails. Make the measurements that can run here. Commit in coherent steps. Finish with a clean git status, run git switch --detach so the branch is free for the reviewers and the fixer, delete your build output, and return the structured result.`

const LENSES = [
  { key: 'correctness', text: `Lens: CORRECTNESS. Hunt for real bugs: logic errors, races, lifetime and resource leaks, error paths, integer and time arithmetic, wrong API use (open the API's source or header and check signatures, ownership and threading rules; Windows and Linux code cannot be compiled here, so read it the way a compiler and the OS would), broken compatibility invariants from CLAUDE.md (capability gating, deny_unknown_fields, ALPN, HELPER_PROTOCOL and pipe names, zero cost by default, no per-packet work), lints the CI will deny (unwrap/expect outside tests, missing_trait_methods impls), interactions with the earlier PRs on the base, and tests that pass for the wrong reason.` },
  { key: 'effect', text: `Lens: EFFECT AND COMPLETENESS. Does the branch implement the whole brief and will it actually have the intended effect for users of those platforms? Check each design step against the diff; find steps missing or done differently without a good reason; check that new tests fail without the change (reason about or run a mutation); check measurement honesty (no invented or unmeasured numbers in docs, CHANGELOG or comments; recorded runs have the environment); check the docs and CHANGELOG describe the behaviour accurately, English and Chinese pairs together; check CLAUDE.md conventions (snafu layers, no thiserror/anyhow, testkit use, no environment writes, AppIcons in Flutter, l10n).` },
]

const reviewPrompt = (pr, base, impl, lens) => `${COMMON}
You review one pull request. Do not commit or change the branch.

${brief(pr, base)}

The implementer reported:
${JSON.stringify(impl, null, 1)}

${lens.text}

The branch is ${branchOf(pr)} (shared refs: run git switch --detach ${branchOf(pr)} in your worktree, then git submodule update --init). Diff: git diff ${base}..${branchOf(pr)}. Read every changed line and the code around it. You may build and run focused tests here. Report only findings you have evidence for; each with severity (blocker: wrong behaviour, crash, CI failure, invariant break; major: a real defect or a missing part of the brief; minor: worth fixing, low impact), file, line, evidence and a concrete fix. Delete your build output before you return the structured result.`

const fixPrompt = (pr, base, impl, reviews) => `${COMMON}
You finish one pull request after review.

${brief(pr, base)}

The implementer reported:
${JSON.stringify(impl, null, 1)}

Reviews:
${JSON.stringify(reviews, null, 1)}

In your worktree: git switch ${branchOf(pr)}, then git submodule update --init. For each finding, first confirm it against the code (reviewers can be wrong); fix every real one, reject the rest with a reason. If the implementer left part of the brief undone, finish it. Re-run the verification commands that apply here and fix what fails. Commit, leave a clean git status, run git switch --detach, delete your build output, and return the structured result.`

async function runPr(pr, base) {
  const impl = await agent(implPrompt(pr, base), { label: `impl:${pr.name}`, phase: 'Implement', schema: IMPL, model: 'opus', isolation: 'worktree' })
  if (!impl) return { name: pr.name, base, failed: 'implement' }
  const reviews = (await parallel(LENSES.map(l => () => agent(reviewPrompt(pr, base, impl, l), { label: `review-${l.key}:${pr.name}`, phase: 'Review', schema: REVIEW, model: 'opus', isolation: 'worktree' })))).filter(Boolean)
  const fix = await agent(fixPrompt(pr, base, impl, reviews), { label: `fix:${pr.name}`, phase: 'Fix', schema: FIX, model: 'opus', isolation: 'worktree' })
  if (!fix) return { name: pr.name, base, impl, reviews, failed: 'fix' }
  return { name: pr.name, branch: branchOf(pr), base, title: pr.title, impl, reviews, fix }
}

const results = await parallel(LANES.map((lane, li) => async () => {
  const out = []
  let base = BASE
  for (const pr of lane) {
    const r = await runPr(pr, base)
    out.push(r)
    if (r.failed || !r.fix.ready) {
      log(`lane ${li + 1} stopped at ${pr.name}: ${r.failed ? 'agent failed at ' + r.failed : 'not ready'}`)
      for (const rest of lane.slice(lane.indexOf(pr) + 1)) out.push({ name: rest.name, skipped: true })
      break
    }
    base = branchOf(pr)
  }
  return out
}))
const flat = results.flatMap((r, i) => r || LANES[i].map(pr => ({ name: pr.name, failed: 'lane' })))
log(`${flat.filter(r => r.fix && r.fix.ready).length} of ${flat.length} PRs ready`)
return flat
