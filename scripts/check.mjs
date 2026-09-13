#!/usr/bin/env node
/**
 * scripts/check.mjs — minimal repository hygiene checks for OTDEL.
 *
 * Scope and honesty notice:
 * - This script uses Node.js >= 22 built-ins ONLY (no dependencies to install).
 * - It is a lightweight *hygiene* check for the design/prototype stage, not a
 *   linter, not a test runner, and NOT an exhaustive secret scanner. It looks
 *   for a small set of obvious, high-confidence problems only.
 * - Real Rust/TypeScript build, lint, and test steps are intentionally not
 *   invoked here because no such manifests exist yet in this repository. See
 *   docs/development.md for what is planned once those manifests land.
 *
 * Only files tracked by git (`git ls-files`) are scanned. New files must be
 * `git add`-ed before this check will see them locally; a PR's CI run checks
 * out the branch with everything already committed, so this is enforced
 * automatically there. See docs/development.md.
 *
 * What it checks, over the set of files tracked by git:
 *   1. Node.js runtime version (>= 22).
 *   2. Forbidden tracked paths: real .env files, private/, data/, materials/,
 *      and .local/ directories/files, *.pdf, *.zip, *.pem, *.key,
 *      docs/worker-runs/. .local/ is git-ignored, but this check still
 *      rejects it if it were ever force-added, since it must never be
 *      committed regardless of .gitignore.
 *   3. Tracked symlinks are rejected outright (any location), so `make
 *      preview` (which serves design/ with a plain static file server) can
 *      never be tricked into following a link that escapes design/.
 *   4. A small set of obvious credential-shaped strings (AWS access key IDs,
 *      PEM private key headers, Slack tokens, GitHub tokens) in tracked text
 *      files. Best-effort only — absence of a hit here is not proof a file
 *      is secret-free. Known binary asset extensions are skipped (not
 *      scanned as text); anything else that cannot be read as text, or that
 *      is larger than the scan size limit, is reported as a FAILURE rather
 *      than silently skipped, so large/unreadable files can't bypass this
 *      scan unnoticed.
 *   5. Unresolved merge-conflict markers in tracked text files.
 *   6. JSON syntax for every tracked *.json file that passed check 4's
 *      symlink/size/readability/NUL validation (rejected files are never
 *      read here). `tsconfig*.json`, `jsconfig*.json` and `.vscode/*.json`
 *      are JSON with Comments by convention: they are parsed with comments
 *      and trailing commas stripped first, so they are really validated
 *      rather than skipped. A tsconfig additionally requires a workflow that
 *      runs a real TypeScript check (see check 10) — this script does not
 *      type-check anything itself. The type-check requirement is satisfied
 *      by a `run:` step invoking tsc/vue-tsc/svelte-check directly or via a
 *      package script whose *contents* run one of them (`npm run check` with
 *      `"check": "tsc -b"`); a script is never accepted for its name alone,
 *      and the detector has a small self-test that runs with the check.
 *   7. JavaScript syntax for every tracked *.js/*.mjs/*.cjs file that passed
 *      the same validation, via `node --check` (rejected files are never
 *      subprocessed here).
 *   8. `git diff --check` (working tree and index) for whitespace errors,
 *      best-effort — only meaningful when there is a local diff to check.
 *   9. Presence and minimal shape (non-empty, starts with a top-level
 *      heading) of the core docs this worker owns. Docs owned by other
 *      parallel work (e.g. docs/block-01-design.md, docs/block-01-plan.md)
 *      are validated only if/when present in this checkout — this worktree
 *      may simply not be synced with main yet, so their absence here is not
 *      itself a failure.
 *  10. Manifest/CI consistency: when a manifest exists, the corresponding
 *      workflow must actually invoke its build/lint/test commands (for Rust:
 *      fmt, clippy, `cargo test --workspace`, and a database for the
 *      integration suite). This replaces the earlier prototype-stage guard,
 *      which failed unconditionally because no real Rust/web jobs existed yet.
 *      It verifies that the commands are present — only CI itself can show
 *      that they pass. See docs/development.md.
 */

import { spawnSync } from 'node:child_process';
import { readFileSync, statSync, lstatSync } from 'node:fs';
import process from 'node:process';

const repoRoot = process.cwd();
const errors = [];
const notes = [];

function fail(message) {
  errors.push(message);
}

function note(message) {
  notes.push(message);
}

function runGit(args) {
  return spawnSync('git', args, { cwd: repoRoot, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
}

function printReportAndExit() {
  console.log('--- OTDEL repository check ---');
  if (notes.length > 0) {
    console.log('\nNotes:');
    for (const n of notes) console.log(`  - ${n}`);
  }
  if (errors.length > 0) {
    console.log(`\nFAILED (${errors.length} issue(s)):`);
    for (const e of errors) console.log(`  ✗ ${e}`);
    console.log('\nmake check: FAIL');
    process.exit(1);
  }
  console.log('\nAll checks passed.');
  console.log('make check: OK');
  process.exit(0);
}

// --- 1. Node.js version ---
const nodeMajor = Number(process.versions.node.split('.')[0]);
if (nodeMajor < 22) {
  fail(`Node.js >= 22 required, found ${process.versions.node}.`);
}

// --- Sanity: inside a git repo ---
const insideRepo = runGit(['rev-parse', '--is-inside-work-tree']);
if (insideRepo.status !== 0 || insideRepo.stdout.trim() !== 'true') {
  fail('Not inside a git repository (git rev-parse --is-inside-work-tree failed).');
  printReportAndExit();
}

// --- 2. Tracked files list ---
const lsFiles = runGit(['ls-files', '-z']);
if (lsFiles.status !== 0) {
  fail(`git ls-files failed: ${lsFiles.stderr.trim()}`);
  printReportAndExit();
}
const trackedFiles = lsFiles.stdout.split('\0').filter(Boolean);
note(`${trackedFiles.length} tracked file(s) scanned.`);

// --- 3. Forbidden tracked paths ---
const forbiddenPatterns = [
  { re: /(^|\/)\.env$/, reason: 'committed .env file (real environment values must never be committed)' },
  {
    re: /(^|\/)\.env\.[^/]+$/,
    allow: /(^|\/)\.env\.example$/,
    reason: 'committed .env.* file other than .env.example',
  },
  { re: /(^|\/)private\//, reason: 'file under a private/ directory' },
  { re: /(^|\/)data\//, reason: 'file under a data/ directory' },
  { re: /(^|\/)materials\//, reason: 'file under a materials/ directory' },
  {
    re: /(^|\/)\.local(\/|$)/,
    reason: 'file under/named .local — must stay local and git-ignored, never committed, even if force-added',
  },
  { re: /\.pdf$/i, reason: 'committed PDF file' },
  { re: /\.zip$/i, reason: 'committed ZIP archive' },
  { re: /\.pem$/i, reason: 'committed PEM key/certificate file' },
  { re: /\.key$/i, reason: 'committed .key file' },
  { re: /(^|\/)docs\/worker-runs\//, reason: 'worker-run status log should stay local/ignored, not committed' },
];

for (const file of trackedFiles) {
  for (const pattern of forbiddenPatterns) {
    if (pattern.allow && pattern.allow.test(file)) continue;
    if (pattern.re.test(file)) {
      fail(`Forbidden tracked file: ${file} — ${pattern.reason}.`);
    }
  }
}

// --- 4. Obvious credential-shaped strings + merge-conflict markers ---
// Best-effort, high-confidence patterns only. Skip binary-ish extensions.
const binaryExt = new Set([
  '.png', '.jpg', '.jpeg', '.gif', '.ico', '.webp',
  '.woff', '.woff2', '.ttf', '.eot',
  '.mp4', '.mp3', '.mov', '.pdf', '.zip',
]);
const credentialPatterns = [
  { re: /AKIA[0-9A-Z]{16}/, label: 'possible AWS access key ID' },
  { re: /-----BEGIN (RSA |EC |OPENSSH |DSA |)PRIVATE KEY-----/, label: 'embedded private key block' },
  { re: /xox[baprs]-[0-9A-Za-z-]{10,}/, label: 'possible Slack token' },
  { re: /gh[pousr]_[0-9A-Za-z]{20,}/, label: 'possible GitHub token' },
];
const conflictMarkerRe = /^(<{7} |={7}$|>{7} )/;
// Generous for source/docs text; anything larger is reported, not silently
// skipped — see the file-loop comment below.
const MAX_SCAN_BYTES = 2 * 1024 * 1024;

// Files that made it through every guard below (not a symlink, a regular
// file, within the size limit, readable as UTF-8 text, no NUL bytes). Later
// steps (JSON/JS syntax checks) must read/subprocess ONLY files in this set —
// never re-derive a file list from `trackedFiles` directly, or a rejected
// symlink/oversized/unreadable file could be followed/read/subprocessed
// after all.
const safeTextFiles = new Set();

for (const file of trackedFiles) {
  // Symlinks are rejected everywhere, regardless of extension: a tracked
  // symlink could point outside the intended directory (e.g. outside
  // design/), and `make preview`'s plain static file server would happily
  // follow it. Use lstat (does not follow the link) to detect this.
  let lst;
  try {
    lst = lstatSync(file);
  } catch (err) {
    fail(`Tracked file is missing/unreadable on disk: ${file} (${err.code ?? err.message}).`);
    continue;
  }
  if (lst.isSymbolicLink()) {
    fail(`Tracked symlink not allowed: ${file} (could let make preview serve/escape files outside design/).`);
    continue;
  }
  if (!lst.isFile()) continue; // e.g. a tracked gitlink/submodule entry

  const dot = file.lastIndexOf('.');
  const ext = dot >= 0 ? file.slice(dot).toLowerCase() : '';
  if (binaryExt.has(ext)) continue; // known binary asset type, not scanned as text

  if (lst.size > MAX_SCAN_BYTES) {
    fail(
      `Tracked file exceeds the ${MAX_SCAN_BYTES}-byte text scan limit and has no recognized ` +
      `binary extension: ${file} (${lst.size} bytes). Rejected rather than silently skipped from ` +
      `credential/conflict-marker scanning — confirm it is intended, or give it a recognized binary extension.`
    );
    continue;
  }

  let content;
  try {
    content = readFileSync(file, 'utf8');
  } catch (err) {
    fail(`Could not read tracked file ${file} as text: ${err.message}`);
    continue;
  }
  if (content.includes(String.fromCharCode(0))) {
    fail(
      `Tracked file ${file} contains binary (NUL) content but has no recognized binary extension. ` +
      `Rejected rather than silently skipped — give it a recognized binary extension or confirm it ` +
      `should be tracked as text.`
    );
    continue;
  }

  safeTextFiles.add(file);

  const lines = content.split('\n');
  lines.forEach((line, idx) => {
    if (conflictMarkerRe.test(line)) {
      fail(`Unresolved merge-conflict marker in ${file}:${idx + 1}.`);
    }
    for (const pattern of credentialPatterns) {
      if (pattern.re.test(line)) {
        fail(`${file}:${idx + 1} — ${pattern.label} (heuristic match, verify manually).`);
      }
    }
  });
}

// --- 5. JSON syntax ---
// Only files already validated by the loop above (safeTextFiles) are parsed:
// a rejected symlink/oversized/unreadable file is never read here, even if
// it happens to end in .json — it was already reported as a failure above.
//
// Some tooling files are JSON with Comments (JSONC) by convention and are
// *valid* that way: TypeScript's own tsconfig.json (and jsconfig.json, and
// editor settings under .vscode/) allow // and /* */ comments and trailing
// commas. Rejecting them as "invalid JSON" would be wrong, and skipping them
// would mean a broken tsconfig passes the check unnoticed. They are therefore
// parsed after comments and trailing commas are removed — still validated,
// just against the grammar they actually use.
const jsoncRe = /(^|\/)(tsconfig[^/]*\.json|jsconfig[^/]*\.json)$|(^|\/)\.vscode\/[^/]+\.json$/i;

/**
 * Remove JSONC comments and trailing commas. String literals (including
 * escaped quotes) are copied through untouched, so a `//` or `/*` inside a
 * string value is never mistaken for a comment.
 */
function stripJsonc(text) {
  const out = [];
  // Output indices of commas that are structural (i.e. not inside a string literal).
  // Only those may be dropped as trailing commas; a comma inside a string value such as
  // "a,}" must survive untouched.
  const structuralCommas = [];
  let i = 0;
  let inString = false;
  while (i < text.length) {
    const ch = text[i];
    if (inString) {
      if (ch === '\\' && i + 1 < text.length) {
        out.push(ch, text[i + 1]);
        i += 2;
        continue;
      }
      if (ch === '"') inString = false;
      out.push(ch);
      i += 1;
      continue;
    }
    if (ch === '"') {
      inString = true;
      out.push(ch);
      i += 1;
      continue;
    }
    if (ch === '/' && text[i + 1] === '/') {
      while (i < text.length && text[i] !== '\n') i += 1;
      continue;
    }
    if (ch === '/' && text[i + 1] === '*') {
      i += 2;
      while (i < text.length && !(text[i] === '*' && text[i + 1] === '/')) i += 1;
      i += 2;
      continue;
    }
    if (ch === ',') structuralCommas.push(out.length);
    out.push(ch);
    i += 1;
  }
  // Drop a structural comma when the next non-whitespace character closes the object or
  // array it belongs to.
  for (const pos of structuralCommas) {
    let j = pos + 1;
    while (j < out.length && /\s/.test(out[j])) j += 1;
    if (j < out.length && (out[j] === '}' || out[j] === ']')) out[pos] = '';
  }
  return out.join('');
}

const jsonFiles = trackedFiles.filter((f) => f.toLowerCase().endsWith('.json') && safeTextFiles.has(f));
let jsoncCount = 0;
for (const file of jsonFiles) {
  const raw = readFileSync(file, 'utf8');
  const isJsonc = jsoncRe.test(file);
  if (isJsonc) jsoncCount += 1;
  try {
    JSON.parse(isJsonc ? stripJsonc(raw) : raw);
  } catch {
    // Deliberately no err.message/excerpt: a JSON parse error can quote the
    // offending file content verbatim, which may include secrets.
    fail(isJsonc ? `Invalid JSONC (comments/trailing commas allowed): ${file}` : `Invalid JSON: ${file}`);
  }
}
note(
  `${jsonFiles.length} JSON file(s) parsed (validated text files only), ` +
  `${jsoncCount} of them as JSONC (tsconfig/jsconfig/.vscode).`
);

// --- 6. JavaScript syntax via node --check ---
// Same restriction: only files in safeTextFiles are ever passed to a
// subprocess here.
const jsFiles = trackedFiles.filter((f) => /\.(mjs|cjs|js)$/i.test(f) && safeTextFiles.has(f));
for (const file of jsFiles) {
  const result = spawnSync(process.execPath, ['--check', file], { cwd: repoRoot, encoding: 'utf8' });
  if (result.status !== 0) {
    // Deliberately no stderr excerpt: node's syntax-error output quotes the
    // offending source line verbatim, which may include secrets.
    fail(`Invalid JavaScript syntax: ${file}`);
  }
}
note(`${jsFiles.length} JavaScript file(s) syntax-checked via node --check (validated text files only).`);

// --- 7. git diff --check (best-effort, local diff only) ---
try {
  const diffCheck = runGit(['diff', '--check']);
  if (diffCheck.status !== 0 && diffCheck.stdout.trim()) {
    fail(`git diff --check found whitespace issues:\n${diffCheck.stdout.trim()}`);
  }
  const diffCheckCached = runGit(['diff', '--cached', '--check']);
  if (diffCheckCached.status !== 0 && diffCheckCached.stdout.trim()) {
    fail(`git diff --cached --check found whitespace issues:\n${diffCheckCached.stdout.trim()}`);
  }
  note('git diff --check run against working tree and index (only meaningful if there is a local diff).');
} catch (err) {
  note(`git diff --check could not run: ${err.message}`);
}

// --- 8. Core docs presence and minimal shape ---
function firstNonEmptyLine(content) {
  return content.split('\n').find((l) => l.trim().length > 0) ?? '';
}

const requiredDocs = [
  'README.md',
  'CONTRIBUTING.md',
  '.editorconfig',
  '.env.example',
  'docs/development.md',
  'AGENTS.md',
  'docs/block-01-spec.md',
];

const optionalDocsOnceExist = [
  'docs/block-01-design.md',
  'docs/block-01-plan.md',
];

for (const doc of requiredDocs) {
  let stat;
  try {
    stat = statSync(doc);
  } catch {
    fail(`Required file missing: ${doc}`);
    continue;
  }
  if (!stat.isFile() || stat.size === 0) {
    fail(`Required file is empty: ${doc}`);
    continue;
  }
  if (doc.endsWith('.md')) {
    const content = readFileSync(doc, 'utf8');
    if (!firstNonEmptyLine(content).startsWith('# ')) {
      fail(`${doc} should start with a top-level "# " heading.`);
    }
  }
}

for (const doc of optionalDocsOnceExist) {
  let stat;
  try {
    stat = statSync(doc);
  } catch {
    note(`${doc} not present in this checkout (owned by other parallel work; may already exist on main).`);
    continue;
  }
  if (!stat.isFile() || stat.size === 0) {
    fail(`${doc} exists but is empty.`);
    continue;
  }
  const content = readFileSync(doc, 'utf8');
  if (!firstNonEmptyLine(content).startsWith('# ')) {
    fail(`${doc} should start with a top-level "# " heading.`);
  }
}

// --- 10. Manifest-vs-CI consistency ---
//
// The prototype-stage guard that used to live here failed unconditionally on the
// presence of any manifest, because there were no real Rust/web CI jobs. The Rust
// workspace and its jobs now exist (`.github/workflows/ci.yml`: rust-check and
// rust-integration), so the guard is replaced by the check it was a placeholder for:
// a manifest must be accompanied by a workflow that actually runs its build/lint/test
// commands.
//
// This is still not a claim that CI passes — only CI can show that. It checks that the
// commands are present, so a manifest cannot be added while the workflow keeps
// reporting the suite as "pending". The Rust side is verified against this repository's
// own workflow; the web side is owned by another workflow file
// (.github/workflows/web.yml) and is accepted from either file.
function fileExists(p) {
  try {
    return statSync(p).isFile();
  } catch {
    return false;
  }
}

function readIfPresent(p) {
  return fileExists(p) ? readFileSync(p, 'utf8') : '';
}

const ciWorkflow = readIfPresent('.github/workflows/ci.yml');
const webWorkflow = readIfPresent('.github/workflows/web.yml');
const allWorkflows = ciWorkflow + '\n' + webWorkflow;

if (fileExists('Cargo.toml')) {
  if (!ciWorkflow) {
    fail('Cargo.toml is present but .github/workflows/ci.yml is missing.');
  } else {
    const requiredRustSteps = [
      { re: /cargo\s+fmt\s+--all\s+--\s+--check/, label: 'cargo fmt --all -- --check' },
      { re: /cargo\s+clippy\s+--workspace\s+--all-targets\s+--\s+-D\s+warnings/, label: 'cargo clippy --workspace --all-targets -- -D warnings' },
      { re: /cargo\s+test\s+--workspace/, label: 'cargo test --workspace' },
    ];
    for (const step of requiredRustSteps) {
      if (!step.re.test(ciWorkflow)) {
        fail(
          `Cargo.toml is present, but .github/workflows/ci.yml does not run \`${step.label}\`. ` +
          `A Rust manifest must come with a job that really builds, lints and tests it. ` +
          `See docs/development.md.`
        );
      }
    }
    // The database-backed suites must actually be provided with a database, otherwise
    // `cargo test --workspace` in CI fails on the missing environment (by design) and
    // somebody would be tempted to make those tests skip silently instead.
    if (!/OTDEL_TEST_DATABASE_URL/.test(ciWorkflow)) {
      fail(
        'Cargo.toml is present, but .github/workflows/ci.yml never sets ' +
        'OTDEL_TEST_DATABASE_URL — the database-backed integration tests would not run. ' +
        'See docs/backend-1a.md.'
      );
    }
  }
  note('Cargo.toml present: checked that ci.yml runs fmt, clippy and the full test suite.');
}

for (const manifest of ['package.json', 'apps/web/package.json']) {
  if (!fileExists(manifest)) continue;
  const hasInstall = /(npm\s+ci|pnpm\s+install|yarn\s+install)/.test(allWorkflows);
  const hasBuildOrTest = /(npm\s+run\s+build|pnpm\s+(run\s+)?build|npm\s+test|npm\s+run\s+test|pnpm\s+(run\s+)?test)/.test(allWorkflows);
  if (!hasInstall || !hasBuildOrTest) {
    fail(
      `${manifest} is present, but no workflow (.github/workflows/ci.yml or web.yml) runs a ` +
      `dependency install plus build/test for it. Add the real job in the same PR as the ` +
      `manifest. See docs/development.md.`
    );
  } else {
    note(`${manifest} present: found install and build/test commands in the workflows.`);
  }
}

// A tsconfig is parsed as JSONC above, which proves only that it is syntactically
// well-formed. The actual TypeScript validation is `tsc` (or vue-tsc/svelte-check), and
// it belongs in a workflow — otherwise relaxing the JSON parse for tsconfig would have
// traded a false failure for a genuinely unchecked frontend.
//
// A workflow rarely calls the compiler directly: the normal shape is a step that runs a
// package script, e.g. `run: npm run check` with `"check": "tsc -b"` in
// apps/web/package.json. That is a real type check and must be recognised — but via the
// script's *contents*, never its name. `"check": "echo ok"` is not a type check no
// matter what the step is called, so the script is looked up and inspected.

/** `scripts` of a manifest, or `{}` when it is absent/unreadable (check 5 reports that). */
function packageScripts(manifestPath) {
  if (!fileExists(manifestPath)) return {};
  try {
    const parsed = JSON.parse(readFileSync(manifestPath, 'utf8'));
    const scripts = parsed && typeof parsed === 'object' ? parsed.scripts : null;
    return scripts && typeof scripts === 'object' ? scripts : {};
  } catch {
    return {};
  }
}

// A command that really invokes the TypeScript compiler: `tsc` in any of its forms
// (`tsc -b`, `tsc --build`, `tsc --noEmit`, `npx tsc -p ...`) or a framework wrapper
// around it. Anchored on shell token boundaries so it does not match inside a longer
// word such as `tscheck` or a path like `./notsc`.
const TS_CHECKER_RE = /(?:^|[\s;&|(])(?:npx\s+|pnpm\s+exec\s+|yarn\s+)?(?:vue-tsc|svelte-check|tsc)(?:$|[\s;&|)])/;

/** Script names invoked from a command line, e.g. `npm run check` -> `check`. */
function referencedScriptNames(command) {
  return [...command.matchAll(/\b(?:npm|pnpm|yarn)\s+run\s+([A-Za-z0-9_:.-]+)/g)].map((m) => m[1]);
}

/**
 * The `run:` commands of a workflow — inline (`run: npm ci`) and block scalars
 * (`run: |` followed by deeper-indented lines).
 *
 * Only these are considered. Step *names* and YAML comments are deliberately excluded:
 * a step called "Type-check (tsc)" proves nothing about what the step executes.
 */
function workflowRunCommands(text) {
  const commands = [];
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i += 1) {
    const match = /^(\s*)(?:-\s+)?run:\s*(.*)$/.exec(lines[i]);
    if (!match) continue;
    const indent = match[1].length;
    const inline = match[2].trim();
    if (inline && !/^[|>][-+]?\d*$/.test(inline)) {
      commands.push(inline);
      continue;
    }
    for (let j = i + 1; j < lines.length; j += 1) {
      if (lines[j].trim() === '') continue;
      if (lines[j].length - lines[j].trimStart().length <= indent) break;
      commands.push(lines[j].trim());
    }
  }
  return commands;
}

/**
 * Find a real TypeScript check among a workflow's `run:` commands.
 *
 * Returns a short description of what was found, or `null`. A package script is followed
 * into the manifests (and one script may call another, with cycle protection); the tool
 * it ends up running is what decides.
 */
function findTypeScriptCheck(workflowText, manifests) {
  const resolve = (name, seen) => {
    if (seen.has(name)) return null;
    seen.add(name);
    for (const [manifestPath, scripts] of manifests) {
      const body = scripts[name];
      if (typeof body !== 'string') continue;
      if (TS_CHECKER_RE.test(body)) return `${manifestPath} script "${name}": ${body}`;
      for (const nested of referencedScriptNames(body)) {
        const found = resolve(nested, seen);
        if (found) return found;
      }
    }
    return null;
  };

  for (const command of workflowRunCommands(workflowText)) {
    if (TS_CHECKER_RE.test(command)) return `workflow step \`${command}\``;
    for (const name of referencedScriptNames(command)) {
      const found = resolve(name, new Set());
      if (found) return found;
    }
  }
  return null;
}

// Tiny self-test of the detector above. It runs on every invocation because the rules it
// encodes are exactly the ones that are easy to break: accepting a script because of its
// name, or accepting a step title that merely mentions tsc.
function selfTestTypeScriptCheckDetector() {
  const scripts = (obj) => [['test-fixture/package.json', obj]];
  const cases = [
    // The real frontend shape: a named script that does run the compiler.
    ['    - name: Type-check\n      run: npm run check\n', scripts({ check: 'tsc -b' }), true],
    ['    - run: pnpm run typecheck\n', scripts({ typecheck: 'vue-tsc --noEmit' }), true],
    // One script delegating to another still resolves to the real tool.
    ['    - run: npm run check\n', scripts({ check: 'npm run tc', tc: 'tsc -b' }), true],
    ['    - run: npm run build\n', scripts({ build: 'tsc -b && vite build' }), true],
    // Direct invocations, with and without a block scalar.
    ['    - run: npx tsc --noEmit\n', scripts({}), true],
    ['    - run: |\n        npm ci\n        svelte-check\n', scripts({}), true],
    // A script is judged by what it runs, never by its name.
    ['    - run: npm run check\n', scripts({ check: 'echo ok' }), false],
    ['    - run: npm run check\n', scripts({}), false],
    // A step title that mentions the compiler is not a type check.
    ['    - name: Type-check (tsc)\n      run: npm run lint\n', scripts({ lint: 'oxlint' }), false],
    // Cycles must not hang or pass.
    ['    - run: npm run a\n', scripts({ a: 'npm run b', b: 'npm run a' }), false],
  ];
  for (const [workflow, manifests, expected] of cases) {
    if (Boolean(findTypeScriptCheck(workflow, manifests)) !== expected) {
      fail(
        `scripts/check.mjs self-test failed: the TypeScript-check detector ${expected ? 'missed' : 'wrongly accepted'} ` +
        `${JSON.stringify(workflow)}. Fix the detector before relying on this check.`
      );
      return null;
    }
  }
  return cases.length;
}

const tsconfigs = trackedFiles.filter((f) => /(^|\/)tsconfig[^/]*\.json$/i.test(f));
if (tsconfigs.length > 0) {
  const selfTested = selfTestTypeScriptCheckDetector();
  if (selfTested) note(`TypeScript-check detector self-test passed (${selfTested} cases).`);
  const manifests = ['apps/web/package.json', 'package.json'].map((m) => [m, packageScripts(m)]);
  const found =
    findTypeScriptCheck(ciWorkflow, manifests) || findTypeScriptCheck(webWorkflow, manifests);
  if (!found) {
    fail(
      `${tsconfigs[0]} is present, but no workflow (.github/workflows/ci.yml or web.yml) runs a ` +
      `TypeScript check. A \`run:\` step must invoke tsc/vue-tsc/svelte-check, either directly ` +
      `(\`npx tsc --noEmit\`) or through a package script (\`npm run check\` with ` +
      `\`"check": "tsc -b"\`). A script is accepted for what it runs, not for its name. This ` +
      `script only validates tsconfig syntax as JSONC; it does not type-check. See ` +
      `docs/development.md.`
    );
  } else {
    note(`${tsconfigs.length} tsconfig file(s) present: TypeScript check found via ${found}.`);
  }
}

printReportAndExit();
