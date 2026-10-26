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
  /** When the process started (not when it began serving): what `ps` reports. */
  started_at: string;
  /** The workspace's real path. */
  workspace: string;
  /** 128 random bits, hex: a `stop` carrying it is obeyed as the first frame, from any build. */
  token: string;
  node_version: string;
}

/** How a client uses the daemon, named by its `hello`. */
export type ConnectionMode = 'probe' | 'warm';

/** Appended to, one `<unix secs> <build id>` line per fatal exit; never truncated. */
export const CRASH_FILE = 'typed.crashes';

/** How long a record's live process may take to answer `hello` before it counts as busy. */
const HELLO_GRACE_MS = 2000;

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
  // The compiler is part of the build: the same sources over another
  // TypeScript answer differently.
  const root = path.resolve(distSrc, '..', '..');
  for (const name of COMPILER_PACKAGES) hash.update(`pkg:${name}\0${installedVersion(root, name)}\n`);
  return hash.digest('hex');
}

/** The packages whose installed version the build id hashes, in order. The Rust client hashes the same. */
const COMPILER_PACKAGES = ['typescript', 'ts-morph', '@ts-morph/common'];

/**
 * The `version` of the first `node_modules/<name>/package.json` from `root`
 * up (Node's lookup, so a hoisted install is found), or `''` when there is
 * none, it does not parse, or its version is not a string.
 */
function installedVersion(root: string, name: string): string {
  for (let dir = root; ; dir = path.dirname(dir)) {
    const manifest = path.join(dir, 'node_modules', name, 'package.json');
    if (fs.existsSync(manifest)) {
      try {
        const version = JSON.parse(fs.readFileSync(manifest, 'utf8'))?.version;
        return typeof version === 'string' ? version : '';
      } catch {
        return '';
      }
    }
    if (path.dirname(dir) === dir) return '';
  }
}

/**
 * Where the daemon for `workspace` listens: `<workspace>/.carrick/typed.sock`,
 * or, when that path is past the platform's socket path limit, a name derived
 * from the workspace's real path in this user's directory under the OS temp
 * directory (`carrick-<uid>/`, 0700, so no other user can bind the name
 * first). Either way the record names it, so clients never derive it
 * themselves.
 */
export function socketPathFor(workspace: string): string {
  const inTree = path.join(workspace, '.carrick', 'typed.sock');
  if (Buffer.byteLength(inTree) <= MAX_SOCKET_PATH_BYTES) return inTree;
  const key = crypto.createHash('sha256').update(realPathOf(workspace)).digest('hex').slice(0, 16);
  const name = `carrick-typed-${key}.sock`;
  const own = `carrick-${process.getuid?.() ?? 'user'}`;
  const inTemp = path.join(os.tmpdir(), own, name);
  return Buffer.byteLength(inTemp) <= MAX_SOCKET_PATH_BYTES ? inTemp : path.join('/tmp', own, name);
}

/** The workspace's real path, as records and `hello` answers name it. */
export function workspaceRealPath(workspace: string): string {
  return realPathOf(workspace);
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

/**
 * What a `hello` to `socket` says of the daemon there: its pid and workspace
 * (a `build_mismatch` answer names them too), `busy` when it accepts but does
 * not answer within `timeoutMs` (a synchronous request holds its event loop),
 * or `none` when nothing accepts.
 */
export function helloIdentity(
  socket: string,
  buildId: string,
  timeoutMs = HELLO_GRACE_MS
): Promise<{ pid: number; workspace: string } | 'busy' | 'none' | 'unknown'> {
  return new Promise((resolve) => {
    const conn = net.createConnection(socket);
    let buffer = '';
    let connected = false;
    const done = (outcome: { pid: number; workspace: string } | 'busy' | 'none' | 'unknown') => {
      clearTimeout(timer);
      conn.destroy();
      resolve(outcome);
    };
    const timer = setTimeout(() => done(connected ? 'busy' : 'none'), timeoutMs);
    conn.setEncoding('utf8');
    conn.once('connect', () => {
      connected = true;
      conn.write(JSON.stringify({ request_id: 'claim', action: 'hello', build_id: buildId, mode: 'probe' }) + '\n');
    });
    conn.on('data', (chunk: string) => {
      buffer += chunk;
      const end = buffer.indexOf('\n');
      if (end === -1) return;
      try {
        const frame = JSON.parse(buffer.slice(0, end)) as { pid?: unknown; workspace?: unknown };
        done(typeof frame.pid === 'number' && typeof frame.workspace === 'string' ? { pid: frame.pid, workspace: frame.workspace } : 'unknown');
      } catch {
        done('unknown');
      }
    });
    conn.once('error', () => done(connected ? 'unknown' : 'none'));
    conn.once('close', () => done(connected ? 'unknown' : 'none'));
  });
}

/** One client connection. Frames written after it is gone are dropped. */
export class Connection {
  /** Set by the first frame, `hello`, when its build id matches. */
  greeted = false;
  /** What its `hello` said it is: a check's probe, or the warm-up. */
  mode: ConnectionMode | null = null;
  /** The dispatcher's per-connection slot (the service this client init'd). */
  slot: unknown = undefined;
  /** Its socket has closed: the client went away, or the daemon closed it. */
  closed = false;

  constructor(private readonly socket: net.Socket) {
    socket.once('close', () => {
      this.closed = true;
    });
  }

  /** Call `listener` once the connection closes; returns how to stop listening. */
  onClose(listener: () => void): () => void {
    if (this.closed) {
      listener();
      return () => {};
    }
    this.socket.once('close', listener);
    return () => this.socket.off('close', listener);
  }

  /** The daemon closed it (`destroy`), rather than the client going away. */
  closedByDaemon = false;

  /** Close at once, dropping any frame not yet sent: the client asks a process of its own. */
  destroy(): void {
    this.closed = true;
    this.closedByDaemon = true;
    this.socket.destroy();
  }

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
  /** A request ran to its end for a client that had gone. */
  onAbandoned?: (conn: Connection) => void;
  log: (message: string) => void;
}

/** What `start` found: this process now serves, or another daemon already does. */
export type StartOutcome = 'serving' | 'already_running';

export class TypedServer {
  readonly socketPath: string;
  readonly recordPath: string;
  readonly crashPath: string;
  /** The process's start, as `ps` reports it: the record's `started_at`. */
  readonly startedAt = new Date(Math.round(performance.timeOrigin));
  /** The workspace's real path. */
  readonly workspaceReal: string;
  /** The secret a first-frame `stop` must carry. */
  readonly token = crypto.randomBytes(16).toString('hex');
  /** Every open connection, for `warming`. */
  readonly connections = new Set<Connection>();
  /** Lines taken off the queue unanswered because their client had gone, and requests that ran for a client gone by their end. */
  readonly counts = { skipped: 0, abandoned: 0 };
  private readonly carrickDir: string;
  private readonly guard: WriteGuard;
  private server: net.Server | null = null;
  private queue: Promise<void> = Promise.resolve();
  /** Lines waiting in the queue. */
  private waiting = 0;
  /** A line is being answered. */
  private running = false;
  /** When a live line last arrived or a request last ended. */
  private lastActivity = Date.now();
  private watch: NodeJS.Timeout | null = null;
  private stopping = false;

  constructor(private readonly opts: TypedServerOptions) {
    this.carrickDir = path.join(opts.workspace, '.carrick');
    this.recordPath = path.join(this.carrickDir, TYPED_RECORD);
    this.crashPath = path.join(this.carrickDir, CRASH_FILE);
    this.socketPath = socketPathFor(opts.workspace);
    this.workspaceReal = realPathOf(opts.workspace);
    // Single-file roots: the guard takes a directory root inside the
    // workspace only beneath a `.carrick` directory, and the daemon writes
    // nothing but these four entries.
    const socketDir = path.dirname(this.socketPath);
    const ownSocketDir = socketDir === this.carrickDir ? [] : [socketDir];
    this.guard = WriteGuard.of({
      files: [this.carrickDir, this.recordPath, this.temporaryRecordPath(), this.crashPath, this.socketPath, ...ownSocketDir],
      protect: [opts.workspace],
    });
  }

  /**
   * Milliseconds since a live line last arrived or a request last ended, or 0
   * while one runs. A line skipped because its client had gone is no activity.
   */
  idleMs(): number {
    return this.running ? 0 : Date.now() - this.lastActivity;
  }

  /** Lines waiting behind the one being answered. */
  queued(): number {
    return this.waiting;
  }

  /** Whether a warm-up connection is open. */
  warming(): boolean {
    for (const conn of this.connections) if (conn.mode === 'warm' && !conn.closed) return true;
    return false;
  }

  /** Append this build's crash line (a fatal exit); never throws. */
  noteCrash(): void {
    try {
      this.guard.appendFile(this.crashPath, `${Math.floor(Date.now() / 1000)} ${this.opts.buildId}\n`);
    } catch {
      // The exit goes on.
    }
  }

  async start(): Promise<StartOutcome> {
    this.guard.mkdir(this.carrickDir);
    if (path.dirname(this.socketPath) !== this.carrickDir) ownUserDir(this.guard, path.dirname(this.socketPath));
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
      workspace: this.workspaceReal,
      token: this.token,
      node_version: process.version,
    };
    const temporary = this.temporaryRecordPath();
    // The token in it stops the daemon: only its owner may read it.
    this.guard.writeFile(temporary, JSON.stringify(record, null, 2) + '\n', 0o600);
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
   * Whether the record names this workspace's live daemon: its pid lives and
   * its socket's `hello` answers with that pid and this workspace's real path,
   * or accepts and does not answer within the grace (busy: a synchronous
   * request holds its event loop). An answer naming another pid or workspace
   * (a record copied with the workspace) is stale and taken over. A live
   * process gets a short grace to bind its socket (it claims first, then
   * listens); a pid that never answers was reused by an unrelated process.
   */
  private async heldByLiveDaemon(): Promise<boolean> {
    const deadline = Date.now() + START_GRACE_MS;
    for (;;) {
      const record = readTypedRecord(this.opts.workspace);
      if (record === null && !fs.existsSync(this.recordPath)) return false;
      if (record !== null && !pidAlive(record.pid)) return false;
      if (record !== null) {
        const identity = await helloIdentity(record.socket, this.opts.buildId);
        if (identity === 'busy') return true;
        if (typeof identity === 'object') return identity.pid === record.pid && identity.workspace === this.workspaceReal;
      }
      if (Date.now() >= deadline) return false;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }

  private accept(socket: net.Socket): void {
    const conn = new Connection(socket);
    this.connections.add(conn);
    socket.once('close', () => this.connections.delete(conn));
    // A client killed mid-request leaves EPIPE/ECONNRESET on its socket; an
    // unhandled one would reach `uncaughtException` and end the daemon. The
    // line reader re-emits its input's errors as its own, so it needs one too.
    socket.on('error', () => {});
    const lines = readline.createInterface({ input: socket, terminal: false });
    lines.on('error', () => {});
    lines.on('line', (line) => this.enqueue(conn, line));
  }

  /**
   * One FIFO for the daemon. A line whose client has gone by the time it is
   * taken off the queue is skipped and counted, and is no activity; a request
   * whose client goes while it runs runs to its end (synchronous compiler
   * work cannot be stopped) and is counted as abandoned.
   */
  private enqueue(conn: Connection, line: string): void {
    this.waiting += 1;
    this.lastActivity = Date.now();
    this.queue = this.queue.then(async () => {
      this.waiting -= 1;
      if (conn.closed) {
        this.counts.skipped += 1;
        return;
      }
      this.running = true;
      try {
        await this.opts.handleLine(conn, line);
      } catch (err) {
        this.opts.log(`request failed: ${err instanceof Error ? err.message : String(err)}`);
      } finally {
        this.running = false;
        this.lastActivity = Date.now();
        if (conn.closed && !conn.closedByDaemon) {
          this.counts.abandoned += 1;
          this.opts.onAbandoned?.(conn);
        }
      }
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

/**
 * This user's own directory for a socket outside the workspace: created 0700,
 * and refused when it exists owned by another user or is not a directory.
 */
function ownUserDir(guard: WriteGuard, dir: string): void {
  guard.mkdir(dir, 0o700);
  const stat = fs.lstatSync(dir);
  if (!stat.isDirectory() || (process.getuid && stat.uid !== process.getuid())) {
    throw new Error(`${dir} is not this user's directory`);
  }
  if ((stat.mode & 0o777) !== 0o700) fs.chmodSync(dir, 0o700);
}

function realPathOf(p: string): string {
  try {
    return fs.realpathSync(p);
  } catch {
    return path.resolve(p);
  }
}
