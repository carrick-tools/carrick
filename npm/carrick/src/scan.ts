// What a build of the index looks like from the outside (carrick#1315).
//
// `carrick index`, `resume` and `refresh` are the Rust binary, and the binary
// writes for two readers at once: a person, and a log file. Run under this
// package with the two streams inherited, a first run opened on a tracing
// banner with eleven `<unset>` fields, then a pair of indicatif spinners
// rewriting a pipe — which arrive as padded fragments sharing one line — and
// closed on the boundary block, which is a per-package census of what the
// deterministic passes could not classify. All three are diagnostics.
//
// So the binary is spawned as a child whose streams this process holds, and
// what it states as a marker is rendered here, through the renderer `carrick
// init` already uses: clack in a terminal, the same lines plain in a pipe.
// Nothing else is drawn. The unparsed text is kept for exactly two readers:
// a run that failed, which prints it, and `--verbose`, which does not come
// through here at all — it inherits the streams and gets the binary's own
// output, banner included.
//
// Two rules decide what a line is, and neither reads its words:
//
//   - A marker (`@carrick-…`) is rendered. `src/progress.rs` is the other
//     half of each of these shapes.
//   - Everything else is buffered. A stdout line that arrives BEFORE the
//     first phase is the command's own answer — what `resume` collected, what
//     it is still waiting for — and is shown. After the first phase, stdout
//     is the map, and the summary marker carries what a person needs of it.
//
// A child that states no phase and no summary was not building anything: a
// refusal, a `resume` with nothing to collect, a help text. Its output is
// written through untouched, because the alternative is swallowing the answer.

import { spawn } from "node:child_process";
import {
  createOutput,
  type InitOutput,
  type StepProgress,
  type StepReport,
} from "./init/output.ts";

/** How far a build's phase has got. Mirrors `progress::PhaseState`. */
export type PhaseState = "started" | "done" | "warned";

/** Mirrors `progress::PhaseUpdate`. */
export type PhaseUpdate = { label: string; state: PhaseState };

/** Mirrors `progress::Update`: how far through a service's files a scan is. */
export type ProgressUpdate = {
  service: string;
  service_index: number;
  service_total: number;
  phase: "files" | "intents";
  done: number;
  total: number;
};

/** Mirrors `progress::ServiceSummary`. */
export type ServiceSummary = {
  name: string;
  routes: number;
  calls: number;
  /** Absent on a summary from a binary older than carrick#1321. */
  functions?: number;
  types?: number;
  routes_without_response_type: number;
};

/** Mirrors `progress::Summary`: what a finished build amounts to. */
export type Summary = {
  services: ServiceSummary[];
  elapsed_secs: number;
  next?: string[];
};

/** One line of a build's output, once it has been read. */
export type Marker =
  | { kind: "phase"; phase: PhaseUpdate }
  | { kind: "progress"; update: ProgressUpdate }
  | { kind: "summary"; summary: Summary }
  | { kind: "notice"; text: string };

const PREFIXES = {
  phase: "@carrick-phase ",
  progress: "@carrick-progress ",
  summary: "@carrick-summary ",
  notice: "@carrick-notice ",
} as const;

/**
 * Read one line of a build's output as a marker, or answer null.
 *
 * Longest prefix wins, so a family whose names share a word cannot be read as
 * one another — the same property `src/progress.rs` holds itself to. A marker
 * whose payload is not the JSON this version expects is not a marker: the
 * renderer draws nothing rather than drawing a guess.
 */
export function parseMarker(line: string): Marker | null {
  const text = line.trimStart();
  for (const [kind, prefix] of Object.entries(PREFIXES)) {
    if (!text.startsWith(prefix)) continue;
    let payload: unknown;
    try {
      payload = JSON.parse(text.slice(prefix.length));
    } catch {
      return null;
    }
    if (payload === null || typeof payload !== "object") return null;
    if (kind === "phase") {
      const phase = payload as PhaseUpdate;
      if (typeof phase.label !== "string") return null;
      return { kind: "phase", phase };
    }
    if (kind === "progress") {
      const update = payload as ProgressUpdate;
      if (typeof update.total !== "number") return null;
      return { kind: "progress", update };
    }
    if (kind === "summary") {
      const summary = payload as Summary;
      if (!Array.isArray(summary.services)) return null;
      return { kind: "summary", summary };
    }
    const notice = payload as { text?: unknown };
    if (typeof notice.text !== "string") return null;
    return { kind: "notice", text: notice.text };
  }
  return null;
}

/** The commands this renders. Everything else runs with its streams inherited. */
export const RENDERED = new Set(["index", "resume", "refresh"]);

/**
 * Whether this invocation is rendered, or run as the binary writes it.
 *
 * `--verbose` is the escape hatch the closing line names, so it must hand the
 * terminal straight to the binary. `--detach` answers with an id and leaves a
 * log file behind — there is no run here to draw. `--help` is text.
 */
export function isRendered(command: string | undefined, args: string[]): boolean {
  if (!command || !RENDERED.has(command)) return false;
  return !args.some(
    (arg) =>
      arg === "--verbose" ||
      arg === "-v" ||
      arg === "--detach" ||
      arg === "--help" ||
      arg === "-h",
  );
}

/** `169.4` as `2m49s`, `32.7` as `32.7s`. Under a minute keeps its tenth. */
export function elapsed(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "0.0s";
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m${Math.round(seconds - minutes * 60)}s`;
}

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

/**
 * The one line a finished build is worth: what the index holds, then the one
 * shortfall in it.
 *
 * Routes and calls alone were the whole line, and a service of 111 routes,
 * 363 functions and 175 types read as `111 routes · 1 call` — which to a
 * developer, and to an agent, is a nearly empty index (carrick#1321). The two
 * largest things it holds are named before the shortfall, because the line's
 * subject is what was built.
 *
 * The shortfall is dropped when it is zero. A line states how many, of which
 * thing, and what to do about it; nothing to do is nothing to say
 * (carrick#1284).
 */
export function summaryLine(summary: Summary): string {
  const total = (pick: (service: ServiceSummary) => number | undefined): number =>
    summary.services.reduce((sum, service) => sum + (pick(service) ?? 0), 0);
  const untyped = total((service) => service.routes_without_response_type);
  const functions = total((service) => service.functions);
  const types = total((service) => service.types);
  const parts = [plural(total((service) => service.routes), "route")];
  if (functions > 0) parts.push(plural(functions, "function"));
  if (types > 0) parts.push(plural(types, "type"));
  // Named for what it is. Every call in the index crosses a service boundary,
  // and "1 call" beside 363 functions reads as one function call.
  parts.push(plural(total((service) => service.calls), "external call"));
  if (untyped > 0) parts.push(`${plural(untyped, "route")} without a response type`);
  return parts.join(" · ");
}

/** What a rendered run closes on: the next step, and nothing about cost. */
export function outroLine(summary: Summary): string {
  const next = summary.next ?? [];
  if (next.length > 0) return next.join(" ");
  return "Your agents can query it now. `carrick index --verbose` shows the full report.";
}

/** What one open phase is counting, for the spinner's label. */
type OpenPhase = {
  label: string;
  startedAt: number;
  resolve: (report: StepReport) => void;
  /** Where the step writes while it runs; a no-op in the plain rendering. */
  say: StepProgress;
  counted: string | null;
  notice: string | null;
};

/**
 * The renderer, as a state machine over the child's lines.
 *
 * Separated from the spawn so a test drives it with a written stream and reads
 * the interactive markers back — a terminal's own bytes are not capturable,
 * and the plain rendering is a different renderer with different lines
 * (carrick#1032).
 */
export class ScanRender {
  private readonly output: InitOutput;
  private readonly version: string;
  /** Renders run in order, whichever event scheduled them. */
  private chain: Promise<void> = Promise.resolve();
  /**
   * The open step's own promise.
   *
   * Awaited at the head of every scheduled render and never inside the chain
   * that starts it: a step ends when its phase ends, and a chain that waited
   * on the step before running the line that ends it would wait forever.
   */
  private stepDone: Promise<unknown> = Promise.resolve();
  private open: OpenPhase | null = null;
  /** The fraction the open phase last stated, for its spinner only. */
  private running: string | null = null;
  /** A finished phase whose line is not drawn yet: the summary may replace it. */
  private held: StepReport | null = null;
  private opened = false;
  private summarised = false;
  /** Lines nobody rendered, for a failure and for nothing else. */
  private readonly buffered: string[] = [];
  /** The command's own answer, said before any phase began. */
  private readonly answered: string[] = [];

  constructor(output: InitOutput, version: string) {
    this.output = output;
    this.version = version;
  }

  /** Whether anything at all was rendered. */
  get rendered(): boolean {
    return this.opened;
  }

  /** What was not rendered, in the order it arrived. */
  get unrendered(): string[] {
    return [...this.answered, ...this.buffered];
  }

  /** One line of the child's stdout. */
  stdout(line: string): void {
    if (this.consume(line)) return;
    // Before the first phase this is the command answering — what `resume`
    // collected, what it is still waiting on. After it, it is the map.
    if (!this.opened && line.trim().length > 0) {
      this.answered.push(line);
      return;
    }
    this.buffered.push(line);
  }

  /** One line of the child's stderr. */
  stderr(line: string): void {
    if (this.consume(line)) return;
    this.buffered.push(line);
  }

  private consume(line: string): boolean {
    const marker = parseMarker(line);
    if (!marker) return false;
    if (marker.kind === "phase") this.phase(marker.phase);
    else if (marker.kind === "progress") this.progress(marker.update);
    else if (marker.kind === "notice") this.notice(marker.text);
    else this.summary(marker.summary);
    return true;
  }

  private begin(): void {
    if (this.opened) return;
    this.opened = true;
    this.schedule(() => {
      this.output.intro(`carrick ${this.version}`);
      for (const line of this.answered) this.output.say(line);
      this.answered.length = 0;
    });
  }

  private phase(update: PhaseUpdate): void {
    if (update.state === "started") {
      this.begin();
      this.close();
      const label = update.label;
      const startedAt = Date.now();
      let resolve: (report: StepReport) => void = () => {};
      const finished = new Promise<StepReport>((settle) => {
        resolve = settle;
      });
      const phase: OpenPhase = {
        label,
        startedAt,
        resolve,
        say: () => {},
        counted: null,
        notice: null,
      };
      this.open = phase;
      this.schedule(() => {
        this.stepDone = this.output.step(
          label,
          (say) => {
            phase.say = say;
            return finished;
          },
          (report) => report,
        );
      });
      return;
    }
    // Finished, but not drawn: a summary arriving next is this phase's real
    // outcome, and the join phase has no line of its own.
    this.held = {
      kind: update.state === "warned" ? "warn" : "done",
      text: this.phaseText(),
    };
  }

  private phaseText(): string {
    if (!this.open) return "";
    const spent = elapsed((Date.now() - this.open.startedAt) / 1000);
    return this.open.counted
      ? `${this.open.label}  ${this.open.counted}, ${spent}`
      : `${this.open.label}  ${spent}`;
  }

  /** Draw the held line, if there is one, and free the phase it belongs to. */
  private close(report?: StepReport): void {
    const open = this.open;
    if (!open) return;
    const outcome = report ?? this.held ?? { kind: "done", text: this.phaseText() };
    this.open = null;
    this.held = null;
    this.running = null;
    // Settling the step's promise is not a render, so it does not queue behind
    // one: it is what lets the render already in the chain finish.
    open.resolve(outcome);
  }

  private progress(update: ProgressUpdate): void {
    if (!this.open) return;
    // The spinner says how far through; the line it stops on says how much
    // there was. A fraction is the answer to "is this moving", and the wrong
    // answer to "what did it cover".
    this.open.counted = `${update.total} ${update.phase}`;
    this.running = `${update.done} of ${update.total} ${update.phase}`;
    this.redraw();
  }

  private notice(text: string): void {
    if (!this.open) {
      this.buffered.push(text);
      return;
    }
    this.open.notice = text;
    this.redraw();
  }

  /** Say where an open phase is while it runs, without ending it. */
  private redraw(): void {
    const open = this.open;
    if (!open) return;
    const counted = this.running ? `: ${this.running}` : "";
    const notice = open.notice ? ` (${open.notice})` : "";
    open.say(`${open.label}${counted}${notice}`);
  }

  private summary(summary: Summary): void {
    this.summarised = true;
    this.begin();
    // The counts ARE the closing phase's outcome, so they take its line
    // rather than adding one under it.
    if (summary.services.length > 0) {
      this.close({ kind: "done", text: summaryLine(summary) });
    } else {
      this.close();
    }
    this.schedule(() => this.output.outro(outroLine(summary)));
  }

  /**
   * The child is over. Answer whether anything was rendered, so a caller that
   * rendered nothing can write the output through instead of eating it.
   */
  async finish(failed: boolean): Promise<boolean> {
    if (this.open) {
      this.close(
        failed
          ? { kind: "refuse", text: `${this.open.label} — the scan stopped` }
          : undefined,
      );
      if (failed) this.summarised = false;
    }
    // One last link, so the chain waits on whatever step is still open.
    this.schedule(() => {});
    await this.chain;
    return this.opened && this.summarised;
  }

  private schedule(work: () => void | Promise<unknown>): void {
    this.chain = this.chain
      .then(() => this.stepDone)
      .then(work)
      .then(
        () => {},
        () => {},
      );
  }
}

/** Where a rendered run writes the lines it did not draw. */
function writeThrough(lines: string[]): void {
  if (lines.length === 0) return;
  process.stderr.write(`${lines.join("\n")}\n`);
}

/**
 * The signals a parent passes on to the scan it is running, and the signal it
 * only waits for (carrick#1391).
 *
 * `SIGTERM` and `SIGHUP` reach the process they name and nothing else, so a
 * harness stopping this command, or a `kill` on its pid, is heard here alone:
 * they are passed on. A terminal's `SIGINT` has already gone to the whole
 * foreground process group, child included, and a second one tells the scanner
 * to abandon the report it is in the middle of making (carrick#1235) — so it
 * is not passed on, and `kill -INT <this pid>` is the case this leaves to the
 * scan's own end.
 */
const PASSED_ON: NodeJS.Signals[] = ["SIGTERM", "SIGHUP"];
const AWAITED: NodeJS.Signals[] = ["SIGINT"];

/**
 * Stay for the child, and tell it what was said to us.
 *
 * Node's default action for each of these ends this process, which is what
 * left a scanner running with nobody holding its streams: orphaned, it wrote
 * on to a pipe whose other end had gone and died of it, with nothing said to
 * the cloud. A listener — any listener — takes that default away, so the
 * important half of this is that it exists at all; what it does with the
 * signal is the rest.
 *
 * The second signal is the way out. Someone who signals twice is asking for
 * this to be over now, so the listeners go, the child is killed rather than
 * left, and the signal is re-raised at this process so the shell reads it as
 * the signal it was.
 *
 * Returns the undo, which the caller runs before it reports the child's own
 * ending: a re-raised signal with these still installed would be caught by
 * them instead of ending anything.
 */
export function relaySignals(
  child: { kill: (signal: NodeJS.Signals) => boolean },
  // Injected so a test can drive the whole shape in one process.
  host: {
    on: (signal: NodeJS.Signals, handler: () => void) => void;
    off: (signal: NodeJS.Signals, handler: () => void) => void;
    raise: (signal: NodeJS.Signals) => void;
  } = {
    on: (signal, handler) => void process.on(signal, handler),
    off: (signal, handler) => void process.off(signal, handler),
    raise: (signal) => void process.kill(process.pid, signal),
  },
): () => void {
  const installed: [NodeJS.Signals, () => void][] = [];
  let heard = false;
  const stop = (): void => {
    for (const [signal, handler] of installed.splice(0)) host.off(signal, handler);
  };
  for (const signal of [...PASSED_ON, ...AWAITED]) {
    const handler = (): void => {
      if (heard) {
        stop();
        try {
          child.kill("SIGKILL");
        } catch {
          // Already gone, which is the state this was asking for.
        }
        host.raise(signal);
        return;
      }
      heard = true;
      if (!PASSED_ON.includes(signal)) return;
      try {
        child.kill(signal);
      } catch {
        // The child ended between the signal and this; its own exit is the
        // answer the caller is already waiting for.
      }
    };
    installed.push([signal, handler]);
    host.on(signal, handler);
  }
  return stop;
}

/**
 * Run the binary and render it.
 *
 * stdin stays inherited: a build asks nothing, but a child holding no stdin
 * would break the moment one of them does.
 */
export async function renderScan(options: {
  binary: string;
  args: string[];
  env: NodeJS.ProcessEnv;
  version: string;
  output?: InitOutput;
}): Promise<{ code: number; signal: NodeJS.Signals | null }> {
  const render = new ScanRender(options.output ?? createOutput(), options.version);
  const child = spawn(options.binary, options.args, {
    stdio: ["inherit", "pipe", "pipe"],
    // The markers are written only when a parent asks for them, so a run
    // nobody is rendering — a CI scan, a direct `carrick index` — is unchanged.
    env: { ...options.env, CARRICK_PROGRESS: "1" },
  });

  const read = (
    stream: NodeJS.ReadableStream,
    take: (line: string) => void,
  ): Promise<void> =>
    new Promise((resolve) => {
      let rest = "";
      stream.setEncoding("utf8");
      stream.on("data", (chunk: string) => {
        const lines = (rest + chunk).split("\n");
        rest = lines.pop() ?? "";
        for (const line of lines) take(line);
      });
      stream.on("end", () => {
        if (rest.length > 0) take(rest);
        resolve();
      });
      stream.on("error", () => resolve());
    });

  // Before the first await on the child, because a signal that lands between
  // the spawn and this one would end this process and orphan it (carrick#1391).
  const stopRelaying = relaySignals(child);

  const ended = new Promise<{ code: number; signal: NodeJS.Signals | null }>(
    (resolve, reject) => {
      child.on("error", reject);
      child.on("exit", (code, signal) => resolve({ code: code ?? 1, signal }));
    },
  );

  const [, , outcome] = await Promise.all([
    read(child.stdout, (line) => render.stdout(line)),
    read(child.stderr, (line) => render.stderr(line)),
    ended,
  ]);
  // The child is gone, so there is nothing left to relay — and the caller
  // answers a signalled child by raising that signal at itself, which these
  // would otherwise catch.
  stopRelaying();

  const failed = outcome.code !== 0 || outcome.signal !== null;
  const drew = await render.finish(failed);
  // A run that drew nothing was not a build: a refusal, a `resume` with
  // nothing to collect, a help text. Its output IS the answer.
  if (!drew) writeThrough(render.unrendered);
  else if (failed) writeThrough(render.unrendered);
  return outcome;
}
