/**
 * Shared test helpers: sidecar process client and fixture paths.
 */

import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
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
