#!/usr/bin/env node
'use strict';

/**
 * DX-501: the dashboard depends on `@wovyr/ui-react` via `file:../sdks/ui-react`
 * (package.json), and resolves `@wovyr/ui-react/web-component` +
 * `dist/src/styles.css` — both of which exist only after that package's own
 * `npm install && npm run build` has run. Nothing upstream of `ng serve`/
 * `ng build`/`ng test` did that automatically, so a clean checkout failed with
 * four TS2307/unresolved-import errors (verified 2026-07-26; CI's own
 * `dashboard` job invokes `ng` directly via `npx`, bypassing npm script hooks,
 * so it hit the identical failure). This script is that missing step, wired
 * into `prestart`/`prebuild`/`pretest`/`prewatch` in package.json and invoked
 * explicitly as its own step in CI.
 *
 * Skips the rebuild when the SDK's dist/ is already newer than its src/ (the
 * common case once a session is warm), so this doesn't slow every `ng serve`.
 */

const { existsSync, statSync, readdirSync } = require('node:fs');
const { spawnSync } = require('node:child_process');
const path = require('node:path');

const sdkDir = path.resolve(__dirname, '..', '..', 'sdks', 'ui-react');
const distEntry = path.join(sdkDir, 'dist', 'src', 'index.js');
const srcDir = path.join(sdkDir, 'src');

function newestMtimeMs(dir) {
  let newest = 0;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    newest = Math.max(newest, entry.isDirectory() ? newestMtimeMs(full) : statSync(full).mtimeMs);
  }
  return newest;
}

function run(cmd, args) {
  const result = spawnSync(cmd, args, { cwd: sdkDir, stdio: 'inherit', shell: process.platform === 'win32' });
  if (result.status !== 0) {
    console.error(`[ensure-ui-react-built] "${cmd} ${args.join(' ')}" failed in ${sdkDir}`);
    process.exit(result.status ?? 1);
  }
}

if (!existsSync(sdkDir)) {
  console.error(`[ensure-ui-react-built] expected sdks/ui-react at ${sdkDir}, not found.`);
  process.exit(1);
}

const needsBuild = !existsSync(distEntry) || newestMtimeMs(srcDir) > statSync(distEntry).mtimeMs;

if (!needsBuild) {
  console.log('[ensure-ui-react-built] @wovyr/ui-react/dist is up to date, skipping rebuild.');
  process.exit(0);
}

console.log('[ensure-ui-react-built] Building @wovyr/ui-react (the dashboard resolves its dist/ output)...');
if (!existsSync(path.join(sdkDir, 'node_modules'))) {
  run('npm', ['install', '--no-audit', '--no-fund']);
}
run('npm', ['run', 'build']);
