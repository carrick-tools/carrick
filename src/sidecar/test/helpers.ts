/**
 * Shared test helpers: sidecar process client and fixture paths.
 */

import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import * as crypto from 'node:crypto';
import * as http from 'node:http';
import * as zlib from 'node:zlib';
import type { Project } from 'ts-morph';
import type { ExpandOrigin } from '../src/type-structural-expander.js';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// When running from dist/test/, go up one level to dist/, then into src/
export const SIDECAR_PATH = path.join(__dirname, '..', 'src', 'index.js');
// Fixtures are in the source test directory, not dist
export const FIXTURES_PATH = path.join(
  __dirname,
  '..',
  '..',
  'test',
  'fixtures',
  'sample-repo'
);

/**
 * Write .d.ts content into a minimal capture-stub-shaped temp dir
 * (`<dir>/types/surface.d.ts`) for the stub-based `resolve_definitions`
 * action. Callers own cleanup (or leave it to the OS temp dir).
 */
export function stubDirFor(dts: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-defres-'));
  fs.mkdirSync(path.join(dir, 'types'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'types', 'surface.d.ts'), dts);
  return dir;
}

/**
 * The `ExpandOrigin` for a test project: the structural printer decides
 * library origin from the program, so every caller has to hand it one.
 * `repoRoot` defaults to the root, which is what an in-memory project's files
 * sit under.
 */
export function expandOriginOf(project: Project, repoRoot = '/'): ExpandOrigin {
  return { program: project.getProgram().compilerObject, repoRoot };
}

// tgz, integrityOf and localRegistry mirror the library store's test support
// in the companion cloud repo (lambdas/library-claims/test_support.js), served
// over HTTP here because the installer is a separate process.
function tarHeader(name: string, size: number): Buffer {
  const h = Buffer.alloc(512);
  h.write(name, 0, 100, 'utf8');
  h.write('0000644\0', 100);
  h.write('0000000\0', 108);
  h.write('0000000\0', 116);
  h.write(`${size.toString(8).padStart(11, '0')}\0`, 124);
  h.write('00000000000\0', 136);
  h.write('        ', 148);
  h.write('0', 156);
  h.write('ustar\0', 257);
  h.write('00', 263);
  let sum = 0;
  for (const byte of h) sum += byte;
  h.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148);
  return h;
}

function tarBlock(data: Buffer): Buffer {
  return Buffer.concat([data, Buffer.alloc((512 - (data.length % 512)) % 512)]);
}

/** A gzipped ustar tarball laid out as npm publishes one: every path under `package/`. */
export function tgz(files: Record<string, string>): Buffer {
  const parts: Buffer[] = [];
  for (const [rel, content] of Object.entries(files)) {
    const body = Buffer.from(content);
    parts.push(tarHeader(`package/${rel}`, body.length), tarBlock(body));
  }
  parts.push(Buffer.alloc(1024));
  return zlib.gzipSync(Buffer.concat(parts));
}

export function integrityOf(bytes: Buffer): string {
  return `sha512-${crypto.createHash('sha512').update(bytes).digest('base64')}`;
}

export interface RegistryPackage {
  name: string;
  version: string;
  /** Merged into the published package.json beside name and version. */
  manifest?: Record<string, unknown>;
  files?: Record<string, string>;
  /** Listed in the packument, but its tarball answers 404. */
  unfetchable?: boolean;
}

export interface LocalRegistry {
  /** Base URL with a trailing slash, for `npm_config_registry`. */
  url: string;
  /** Tarball paths requested, in order. */
  tarballs: string[];
  close(): Promise<void>;
}

/**
 * An npm registry on 127.0.0.1 serving packuments and tarballs for the given
 * packages, so an installer runs offline against a fixed set of versions.
 */
export async function localRegistry(packages: RegistryPackage[]): Promise<LocalRegistry> {
  const packuments = new Map<string, { name: string; 'dist-tags': Record<string, string>; versions: Record<string, unknown> }>();
  const tarballs = new Map<string, Buffer | null>();
  const requested: string[] = [];
  const server = http.createServer((req, res) => {
    const urlPath = (req.url ?? '/').split('?')[0];
    if (tarballs.has(urlPath)) {
      requested.push(urlPath);
      const bytes = tarballs.get(urlPath);
      if (!bytes) {
        res.writeHead(404, { 'content-type': 'application/json' });
        res.end('{}');
        return;
      }
      res.writeHead(200, { 'content-type': 'application/octet-stream' });
      res.end(bytes);
      return;
    }
    const doc = packuments.get(decodeURIComponent(urlPath.slice(1)));
    res.writeHead(doc ? 200 : 404, { 'content-type': 'application/json' });
    res.end(JSON.stringify(doc ?? {}));
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as { port: number };
  const base = `http://127.0.0.1:${port}`;
  for (const pkg of packages) {
    const manifest = { name: pkg.name, version: pkg.version, ...(pkg.manifest ?? {}) };
    const bytes = tgz({ 'package.json': JSON.stringify(manifest), ...(pkg.files ?? {}) });
    const tarballPath = `/${pkg.name}/-/${pkg.name.split('/').pop()}-${pkg.version}.tgz`;
    tarballs.set(tarballPath, pkg.unfetchable ? null : bytes);
    const doc = packuments.get(pkg.name) ?? { name: pkg.name, 'dist-tags': {}, versions: {} };
    doc.versions[pkg.version] = {
      ...manifest,
      dist: { tarball: `${base}${tarballPath}`, integrity: integrityOf(bytes) },
    };
    doc['dist-tags'].latest = pkg.version;
    packuments.set(pkg.name, doc);
  }
  return {
    url: `${base}/`,
    tarballs: requested,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

/**
 * Point every installer this process spawns at `registry`, with a store and
 * cache under `scratch`, so the developer's own store is never read. Returns
 * the restore.
 */
export function useRegistry(registry: LocalRegistry, scratch: string): () => void {
  const values: Record<string, string> = {
    npm_config_registry: registry.url,
    npm_config_store_dir: path.join(scratch, 'store'),
    npm_config_cache_dir: path.join(scratch, 'cache'),
  };
  const previous = Object.fromEntries(Object.keys(values).map((k) => [k, process.env[k]]));
  Object.assign(process.env, values);
  return () => {
    for (const [k, v] of Object.entries(previous)) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
  };
}

/**
 * Helper class to manage sidecar process communication
 */
export class SidecarClient {
  private process: ChildProcessWithoutNullStreams | null = null;
  private responseBuffer: string = '';
  private responsePromises: Array<{
    resolve: (response: unknown) => void;
    reject: (error: Error) => void;
  }> = [];

  /**
   * Start the sidecar process
   */
  async start(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.process = spawn('node', [SIDECAR_PATH], {
        stdio: ['pipe', 'pipe', 'pipe'],
      });

      // Handle stdout (JSON responses)
      this.process.stdout.on('data', (data: Buffer) => {
        this.responseBuffer += data.toString();

        // Process complete lines
        const lines = this.responseBuffer.split('\n');
        this.responseBuffer = lines.pop() || '';

        for (const line of lines) {
          if (line.trim()) {
            try {
              const response = JSON.parse(line);
              // A progress frame is not an answer (carrick#1914).
              if (response?.status === 'progress') continue;
              const promise = this.responsePromises.shift();
              if (promise) {
                promise.resolve(response);
              }
            } catch (err) {
              console.error('Failed to parse response:', line);
            }
          }
        }
      });

      // Handle stderr (logs)
      this.process.stderr.on('data', (data: Buffer) => {
        // Log to console for debugging
        const msg = data.toString().trim();
        if (msg) {
          console.error('[sidecar stderr]', msg);
        }
      });

      this.process.on('error', (err) => {
        reject(err);
      });

      // Give it a moment to start up
      setTimeout(resolve, 100);
    });
  }

  /**
   * Send a request and wait for response
   */
  async send<T = unknown>(
    request: Record<string, unknown>,
    timeoutMs: number = 10000
  ): Promise<T> {
    if (!this.process) {
      throw new Error('Sidecar not started');
    }

    return new Promise((resolve, reject) => {
      this.responsePromises.push({ resolve: resolve as (r: unknown) => void, reject });

      const json = JSON.stringify(request);
      this.process!.stdin.write(json + '\n');

      // Timeout (default 10s; compiler-heavy actions like capture_v2 build
      // multiple ts programs and need longer on slow CI runners).
      const timer = setTimeout(() => {
        const index = this.responsePromises.findIndex((p) => p.resolve === resolve);
        if (index !== -1) {
          this.responsePromises.splice(index, 1);
          reject(new Error('Request timeout'));
        }
      }, timeoutMs);
      timer.unref();
    });
  }

  /**
   * Stop the sidecar process
   */
  async stop(): Promise<void> {
    if (this.process) {
      try {
        await this.send({ action: 'shutdown', request_id: 'shutdown' });
      } catch {
        // Ignore shutdown errors
      }
      this.process.kill();
      this.process = null;
    }
  }
}
