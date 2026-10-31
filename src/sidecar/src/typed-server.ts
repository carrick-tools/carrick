/**
 * Server mode: one resident sidecar per workspace (carrick#2879).
 *
 * `node dist/src/index.js --serve <workspace>` listens on a Unix socket instead
 * of reading stdin, so the programs a scan builds outlive the scan. This file
 * owns the transport and the lifecycle only: the socket, the record other
 * processes find the daemon by (`<workspace>/.carrick/typed.json`), the build
 * id handshake, the one queue every request runs through, idle stop, and
 * clean-up. What a request answers is `index.ts`'s, as over stdio.
 *
 * Lifecycle rules:
 * - one daemon per workspace: `typed.json` is claimed by a hard link, which
 *   only one process can win; a record whose process is dead, or whose socket
 *   never answers, is stale and taken over;
 * - it stops on `stop`, on SIGTERM/SIGINT, after `idleMs` with no request, and
 *   when `typed.json` is gone or names another process (the `.carrick/`
 *   directory was removed, or another daemon took over). It removes the
 *   socket and the record only while the record is its own;
 * - a client that goes away mid-request never takes the daemon with it.
 */

import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as net from 'node:net';
import * as os from 'node:os';
import * as path from 'node:path';
import * as readline from 'node:readline';
import { WriteGuard } from './capture/index.js';

/** What `<workspace>/.carrick/typed.json` holds. */
export interface TypedRecord {
  pid: number;
  socket: string;
  build_id: string;
  cli_version: string | null;
  started_at: string;
}

/** The record's name inside the workspace's `.carrick/` directory. */
export const TYPED_RECORD = 'typed.json';

/** Default idle stop: 15 minutes with no request. */
export const DEFAULT_IDLE_MS = 15 * 60 * 1000;

/** Exit code of a second daemon started while one already answers. */
export const EXIT_ALREADY_RUNNING = 3;

/**
 * The longest socket path the platform binds. macOS allows 104 bytes with the
 * terminating NUL, Linux 108; the smaller bound holds on both.
 */
const MAX_SOCKET_PATH_BYTES = 103;

/** How long a record whose process lives may go without its socket answering. */
const START_GRACE_MS = 2000;

/**
 * The build id of the sidecar whose compiled sources sit in `distSrc`: sha256
 * over every file beneath it, by relative path in sorted order. Two installs
 * answer alike only when this is equal, so a client sends the id of the
 * sidecar it would have spawned and a daemon on any other build refuses it.
 */
export function buildIdOf(distSrc: string): string {
  const files: string[] = [];
  const walk = (dir: string): void => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const abs = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(abs);
      else if (entry.isFile()) files.push(abs);
    }
  };
  walk(distSrc);
  const hash = crypto.createHash('sha256');
  for (const rel of files.map((f) => path.relative(distSrc, f).split(path.sep).join('/')).sort()) {
    const content = crypto.createHash('sha256').update(fs.readFileSync(path.join(distSrc, rel))).digest('hex');
    hash.update(`${rel}\0${content}\n`);
  }
  return hash.digest('hex');
}

/**
 * Where the daemon for `workspace` listens: `<workspace>/.carrick/typed.sock`,
 * or, when that path is past the platform's socket path limit, a name under
 * the OS temp directory derived from the workspace's real path. Either way
 * the record names it, so clients never derive it themselves.
 */
export function socketPathFor(workspace: string): string {
  const inTree = path.join(workspace, '.carrick', 'typed.sock');
  if (Buffer.byteLength(inTree) <= MAX_SOCKET_PATH_BYTES) return inTree;
  const key = crypto.createHash('sha256').update(realPathOf(workspace)).digest('hex').slice(0, 16);
  const name = `carrick-typed-${key}.sock`;
  const inTemp = path.join(os.tmpdir(), name);
  return Buffer.byteLength(inTemp) <= MAX_SOCKET_PATH_BYTES ? inTemp : path.join('/tmp', name);
}

/** The record for `workspace`, or null when there is none or it does not parse. */
export function readTypedRecord(workspace: string): TypedRecord | null {
  try {
    const parsed = JSON.parse(fs.readFileSync(path.join(workspace, '.carrick', TYPED_RECORD), 'utf8'));
    if (typeof parsed?.pid !== 'number' || typeof parsed?.socket !== 'string') return null;
    return parsed as TypedRecord;
  } catch {
    return null;
  }
}

/** Whether process `pid` exists (EPERM: it does, owned by someone else). */
export function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    return (err as NodeJS.ErrnoException).code === 'EPERM';
  }
}

/** Whether something accepts a connection on `socket` within `timeoutMs`. */
export function socketAnswers(socket: string, timeoutMs = 500): Promise<boolean> {
  return new Promise((resolve) => {
    const conn = net.createConnection(socket);
    const done = (ok: boolean) => {
      clearTimeout(timer);
      conn.destroy();
      resolve(ok);
    };
    const timer = setTimeout(() => done(false), timeoutMs);
    conn.once('connect', () => done(true));
    conn.once('error', () => done(false));
  });
}

/** One client connection. Frames written after it is gone are dropped. */
export class Connection {
  /** Set by the first frame, `hello`, when its build id matches. */
  greeted = false;
  /** The dispatcher's per-connection slot (the service this client init'd). */
  slot: unknown = undefined;

  constructor(private readonly socket: net.Socket) {}

  write(frame: string): void {
    if (this.socket.destroyed || !this.socket.writable) return;
    this.socket.write(frame);
  }

  /** Write `frame` and resolve once it has left (or the client is gone). */
  flush(frame: string): Promise<void> {
    if (this.socket.destroyed || !this.socket.writable) return Promise.resolve();
    return new Promise((resolve) => this.socket.write(frame, () => resolve()));
  }

  /** Close after every frame already written has been sent. */
  close(): void {
    if (!this.socket.destroyed) this.socket.end();
  }
}

export interface TypedServerOptions {
  workspace: string;
  buildId: string;
  cliVersion: string | null;
  idleMs: number;
  /** Answer one line from `conn`. Runs alone: the server queues every line. */
  handleLine: (conn: Connection, line: string) => Promise<void>;
  log: (message: string) => void;
}

/** What `start` found: this process now serves, or another daemon already does. */
export type StartOutcome = 'serving' | 'already_running';

export class TypedServer {
  readonly socketPath: string;
  readonly recordPath: string;
  readonly startedAt = new Date();
  private readonly carrickDir: string;
  private readonly guard: WriteGuard;
  private server: net.Server | null = null;
  private queue: Promise<void> = Promise.resolve();
  private pending = 0;
  private lastActivity = Date.now();
  private watch: NodeJS.Timeout | null = null;
  private stopping = false;

  constructor(private readonly opts: TypedServerOptions) {
    this.carrickDir = path.join(opts.workspace, '.carrick');
    this.recordPath = path.join(this.carrickDir, TYPED_RECORD);
    this.socketPath = socketPathFor(opts.workspace);
    // Single-file roots: the guard takes a directory root inside the
    // workspace only beneath a `.carrick` directory, and the daemon writes
    // nothing but these four entries.
    this.guard = WriteGuard.of({
      files: [this.carrickDir, this.recordPath, this.temporaryRecordPath(), this.socketPath],
      protect: [opts.workspace],
    });
  }

  /** Milliseconds since the last request finished, or 0 while one runs. */
  idleMs(): number {
    return this.pending > 0 ? 0 : Date.now() - this.lastActivity;
  }

  async start(): Promise<StartOutcome> {
    this.guard.mkdir(this.carrickDir);
    if (!(await this.claim())) return 'already_running';
    if (fs.existsSync(this.socketPath)) this.guard.unlink(this.socketPath);
    // The socket file takes the process umask when it is bound: only its
    // owner may connect.
    const server = net.createServer((socket) => this.accept(socket));
    this.server = server;
    await new Promise<void>((resolve, reject) => {
      server.once('error', reject);
      const umask = process.umask(0o177);
      try {
        server.listen(this.socketPath, () => {
          server.off('error', reject);
          resolve();
        });
      } finally {
        process.umask(umask);
      }
    });
    process.on('exit', () => this.cleanUp());
    const every = Math.max(50, Math.min(1000, Math.floor(this.opts.idleMs / 4)));
    this.watch = setInterval(() => this.check(), every);
    this.opts.log(`serving ${this.opts.workspace} on ${this.socketPath} (build ${this.opts.buildId.slice(0, 12)})`);
    return 'serving';
  }

  /** Stop serving, remove the socket and the record while they are ours, and exit. */
  stop(reason: string): void {
    if (this.stopping) return;
    this.stopping = true;
    this.opts.log(`stopping: ${reason}`);
    if (this.watch) clearInterval(this.watch);
    this.cleanUp();
    this.server?.close();
    // Frames already queued on open connections still go out.
    setImmediate(() => process.exit(0));
  }

  private temporaryRecordPath(): string {
    return `${this.recordPath}.${process.pid}.tmp`;
  }

  /**
   * Take the record. A link onto the record path is the one operation two
   * starting daemons cannot both win; the loser reads the winner's record.
   */
  private async claim(): Promise<boolean> {
    const record: TypedRecord = {
      pid: process.pid,
      socket: this.socketPath,
      build_id: this.opts.buildId,
      cli_version: this.opts.cliVersion,
      started_at: this.startedAt.toISOString(),
    };
    const temporary = this.temporaryRecordPath();
    this.guard.writeFile(temporary, JSON.stringify(record, null, 2) + '\n');
    try {
      for (let attempt = 0; attempt < 3; attempt += 1) {
        try {
          this.guard.link(temporary, this.recordPath);
          return true;
        } catch (err) {
          if ((err as NodeJS.ErrnoException).code !== 'EEXIST') throw err;
        }
        if (await this.heldByLiveDaemon()) return false;
        this.opts.log('taking over a stale typed.json');
        try {
          this.guard.unlink(this.recordPath);
        } catch (err) {
          if ((err as NodeJS.ErrnoException).code !== 'ENOENT') throw err;
        }
      }
      return false;
    } finally {
      this.guard.unlink(temporary);
    }
  }

  /**
   * Whether the record names a daemon that answers. A live process gets a
   * short grace to bind its socket (it claims first, then listens); a pid
   * that never answers was reused by an unrelated process.
   */
  private async heldByLiveDaemon(): Promise<boolean> {
    const deadline = Date.now() + START_GRACE_MS;
    for (;;) {
      const record = readTypedRecord(this.opts.workspace);
      if (record === null && !fs.existsSync(this.recordPath)) return false;
      if (record !== null && !pidAlive(record.pid)) return false;
      if (record !== null && (await socketAnswers(record.socket))) return true;
      if (Date.now() >= deadline) return false;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }

  private accept(socket: net.Socket): void {
    const conn = new Connection(socket);
    // A client killed mid-request leaves EPIPE/ECONNRESET on its socket; an
    // unhandled one would reach `uncaughtException` and end the daemon. The
    // line reader re-emits its input's errors as its own, so it needs one too.
    socket.on('error', () => {});
    const lines = readline.createInterface({ input: socket, terminal: false });
    lines.on('error', () => {});
    lines.on('line', (line) => this.enqueue(conn, line));
  }

  private enqueue(conn: Connection, line: string): void {
    this.pending += 1;
    this.queue = this.queue
      .then(() => this.opts.handleLine(conn, line))
      .catch((err) => this.opts.log(`request failed: ${err instanceof Error ? err.message : String(err)}`))
      .finally(() => {
        this.pending -= 1;
        this.lastActivity = Date.now();
      });
  }

  private check(): void {
    const record = readTypedRecord(this.opts.workspace);
    if (record === null || record.pid !== process.pid) {
      this.opts.log('typed.json is gone or names another process');
      this.stopping = true;
      process.exit(0);
    }
    if (this.idleMs() >= this.opts.idleMs) this.stop(`idle for ${this.opts.idleMs} ms`);
  }

  /** Remove the socket and the record, only while the record names this process. */
  private cleanUp(): void {
    const record = readTypedRecord(this.opts.workspace);
    if (record === null || record.pid !== process.pid) return;
    for (const file of [this.socketPath, this.recordPath]) {
      try {
        this.guard.unlink(file);
      } catch {
        // Already gone.
      }
    }
  }
}

function realPathOf(p: string): string {
  try {
    return fs.realpathSync(p);
  } catch {
    return path.resolve(p);
  }
}
