#!/usr/bin/env node
/**
 * Build the surface lister artifact (carrick#1660): the built sidecar
 * (`dist/src/index.js`) as one self-contained ESM file, and the manifest the
 * cloud's library store pins it by.
 *
 *   npm ci --prefix lister   # once: esbuild, kept out of the sidecar's own install
 *   node lister/build.mjs --tag <release tag> --source-sha <commit> [--out <dir>]
 *
 * Writes two flat files into `--out` (default `dist/lister`):
 * - `carrick-lister.mjs`: every dependency inlined, the TypeScript default
 *   library declarations included (ts-morph carries them in memory). It reads
 *   nothing but itself and the package directory it is given. It speaks the
 *   sidecar's stdio protocol unchanged: `init` answers `ready`,
 *   `list_library_surface` answers `success` with `surfaces` and
 *   `surface_sha256`.
 * - `carrick-lister.manifest.json`: `{ tag, source_sha, entry, node_floor,
 *   protocol, files: { name: sha256 } }`. The pin copies `tag`, `source_sha`,
 *   `entry` and `files`.
 *
 * Run `npm run build` first: the bundle is made from the same compiled
 * output the tests and the scanner run. esbuild has a manifest of its own
 * here so the sidecar's install, which every Action run repeats, does not
 * carry a bundler only a release needs.
 */
import { build } from 'esbuild';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

export const ENTRY = 'carrick-lister.mjs';
export const MANIFEST = 'carrick-lister.manifest.json';
/** The oldest Node major the artifact runs on: the store's `nodejs22.x`. */
export const NODE_FLOOR = '22';
/** The sidecar's stdio protocol (src/sidecar/README.md, "Message Protocol"). */
export const PROTOCOL = 'sidecar-stdio';

const sidecarRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// The compiled sidecar is ESM; TypeScript and ts-morph are CommonJS and read
// `require`, `__filename` and `__dirname` when they load. An ESM file has none
// of the three, so the bundle defines them from its own location. The names
// are only ever read by the CommonJS modules esbuild wraps.
const BANNER = [
  "import { createRequire as __carrickCreateRequire } from 'node:module';",
  "import { fileURLToPath as __carrickFileURLToPath } from 'node:url';",
  "import { dirname as __carrickDirname } from 'node:path';",
  'const require = __carrickCreateRequire(import.meta.url);',
  'const __filename = __carrickFileURLToPath(import.meta.url);',
  'const __dirname = __carrickDirname(__filename);',
].join('\n');

const sha256 = (buffer) => createHash('sha256').update(buffer).digest('hex');

/** Build the artifact into `outDir`; returns the manifest. */
export async function buildLister({ tag, sourceSha, outDir }) {
  if (!tag || !sourceSha) throw new Error('lister/build.mjs: --tag and --source-sha are required');
  const entryPoint = join(sidecarRoot, 'dist', 'src', 'index.js');
  if (!existsSync(entryPoint)) throw new Error(`lister/build.mjs: ${entryPoint} is missing; run npm run build first`);
  mkdirSync(outDir, { recursive: true });
  const outfile = join(outDir, ENTRY);
  await build({
    entryPoints: [entryPoint],
    // Paths esbuild writes into its comments are relative to this, so the
    // bytes do not depend on where the script is run from.
    absWorkingDir: sidecarRoot,
    bundle: true,
    platform: 'node',
    format: 'esm',
    target: 'node22',
    outfile,
    banner: { js: BANNER },
    charset: 'utf8',
    legalComments: 'eof',
    logLevel: 'warning',
  });
  const manifest = {
    tag,
    source_sha: sourceSha,
    entry: ENTRY,
    node_floor: NODE_FLOOR,
    protocol: PROTOCOL,
    files: { [ENTRY]: sha256(readFileSync(outfile)) },
  };
  writeFileSync(join(outDir, MANIFEST), `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const { values } = parseArgs({
    options: {
      tag: { type: 'string' },
      'source-sha': { type: 'string' },
      out: { type: 'string', default: join(sidecarRoot, 'dist', 'lister') },
    },
  });
  const outDir = resolve(values.out);
  const manifest = await buildLister({ tag: values.tag, sourceSha: values['source-sha'], outDir });
  console.log(`lister/build.mjs: ${join(outDir, ENTRY)} (${manifest.files[ENTRY]})`);
}
