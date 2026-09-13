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
 *      read here).
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
 *  10. Manifest/CI enforcement gate: deliberately and unconditionally FAILS
 *      if Cargo.toml, package.json, or apps/web/package.json is present —
 *      there is no real Rust/web CI job in this repo yet, and no attempt to
 *      infer one by pattern-matching workflow text (too easy to fake with a
 *      comment). Adding a manifest requires replacing this guard with a real
 *      CI job in the same PR. See docs/development.md.
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
const jsonFiles = trackedFiles.filter((f) => f.toLowerCase().endsWith('.json') && safeTextFiles.has(f));
for (const file of jsonFiles) {
  try {
    JSON.parse(readFileSync(file, 'utf8'));
  } catch {
    // Deliberately no err.message/excerpt: a JSON parse error can quote the
    // offending file content verbatim, which may include secrets.
    fail(`Invalid JSON: ${file}`);
  }
}
note(`${jsonFiles.length} JSON file(s) parsed (validated text files only).`);

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

// --- 10. Manifest-vs-CI enforcement gate (deliberately dumb, on purpose) ---
// Prototype-stage policy: this repository has NO real Rust/web CI jobs yet,
// so the mere presence of a manifest means the "pending" CI notices are now
// stale and this guard itself needs to be replaced. There is no attempt to
// infer whether a real job "looks right" by pattern-matching workflow text
// (that was tried before and is easy to satisfy with a comment or an example
// snippet without any job actually running) — it simply fails, unconditionally,
// until a human/reviewer replaces this whole gate with a real one in the same
// PR that adds the manifest. See docs/development.md.
function fileExists(p) {
  try {
    return statSync(p).isFile();
  } catch {
    return false;
  }
}

for (const manifest of ['Cargo.toml', 'package.json', 'apps/web/package.json']) {
  if (fileExists(manifest)) {
    fail(
      `${manifest} is present, but this repository has no real CI job for it yet. This is a ` +
      `deliberately unconditional prototype-stage guard (scripts/check.mjs), not a smart check — ` +
      `replace it, and add the real cargo/npm/pnpm build+lint+test job to ` +
      `.github/workflows/ci.yml, in the SAME PR that adds ${manifest}. See docs/development.md.`
    );
  }
}

printReportAndExit();
