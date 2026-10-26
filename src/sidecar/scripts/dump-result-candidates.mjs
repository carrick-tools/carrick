#!/usr/bin/env node
// Prints, for each package given, every export, receiver, member and signature
// with the `result_candidates` the lister lists for it (carrick#2817).
//
//   node scripts/dump-result-candidates.mjs [--max-entries N] <dir> <specifier>...
//
// The lister's entry cap (`max_entries`) truncates a large package's
// signatures; raise it to see every member's candidates.
//
// <dir> is a directory whose node_modules holds the packages. Build the
// sidecar first (`npm run build`); this speaks its stdio protocol the way the
// library store's lister does: `init`, then `list_library_surface`.

import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const args = process.argv.slice(2);
let maxEntries;
if (args[0] === '--max-entries') {
  maxEntries = Number(args[1]);
  args.splice(0, 2);
}
const [dir, ...packages] = args;
if (dir === undefined || packages.length === 0) {
  console.error('usage: dump-result-candidates.mjs [--max-entries N] <dir> <specifier>...');
  process.exit(2);
}
const root = path.resolve(dir);
const entry = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'dist', 'src', 'index.js');

const child = spawn('node', [entry], { stdio: ['pipe', 'pipe', 'inherit'] });
const lines = createInterface({ input: child.stdout });
const waiting = [];
lines.on('line', line => {
  let frame;
  try {
    frame = JSON.parse(line);
  } catch {
    return;
  }
  if (frame.status === 'progress') return;
  waiting.shift()?.(frame);
});
const ask = request =>
  new Promise(resolve => {
    waiting.push(resolve);
    child.stdin.write(`${JSON.stringify(request)}\n`);
  });

const text = candidates =>
  candidates === undefined
    ? '-'
    : `whole_decided=${candidates.whole_decided} members=[${candidates.members.join(',')}] parse=[${candidates.parse.join(',')}]`;

try {
  const ready = await ask({ request_id: 'init', action: 'init', repo_root: root });
  if (ready.status !== 'ready') throw new Error(`init: ${JSON.stringify(ready)}`);
  const listed = await ask({ request_id: 'list', action: 'list_library_surface', from_dir: root,
    packages,
    ...(maxEntries === undefined ? {} : { max_entries: maxEntries }),
  });
  if (listed.status !== 'success') throw new Error(`list_library_surface: ${JSON.stringify(listed)}`);
  for (const surface of listed.surfaces) {
    if (surface.reason !== undefined) {
      console.log(`${surface.package}\tunlisted\t${surface.reason}`);
      continue;
    }
    for (const exported of surface.exports) {
      for (const receiver of exported.receivers) {
        const rows = [
          ...(receiver.call ?? []).map(signature => ['(call)', signature]),
          ...(receiver.construct ?? []).map(signature => ['(new)', signature]),
          ...receiver.members.flatMap(member => member.signatures.map(signature => [member.name, signature])),
        ];
        for (const [member, signature] of rows) {
          console.log([surface.package, exported.export, receiver.receiver, member, text(signature.result_candidates)].join('\t'));
        }
      }
    }
  }
} finally {
  child.stdin.end();
  child.kill();
}
