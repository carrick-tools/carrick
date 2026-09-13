// What `carrick init` puts on the terminal, and how.
//
// One line per thing that happened (carrick#1026). The reasoning, the
// alternatives and the by-hand paths live in the quickstart
// (https://docs.carrick.tools/quickstart); what stays here is the record of
// this run: what was signed in as, what was connected, what was derived, what
// was configured, and what this machine now holds.
//
// Three markers, and nothing else carries state:
//
//   ◇  done — a thing this run did
//   ▲  warning — done, but something is off, and the line says what to do
//   ■  refusal — not done, and the line says why
//
// They are `@clack/prompts`'s own symbols, so the interactive rendering is the
// library's and the plain one is the same characters without its gutter.
//
// Two renderings, chosen by the terminal, never by a flag:
//
// - **Interactive**, when stdout is a TTY: clack draws the gutter, colours the
//   markers, spins during the hosted download and asks the confirm.
// - **Plain**, otherwise — CI, an agent's shell, a pipe. The same lines, one
//   per line, no spinner, no colour, no box. Every executable test runs this
//   one, so the lines a test pins are the lines a scripted caller sees.
//
// A spinner writes and rewrites one line, so anything else printing while it
// spins corrupts it: `step()` owns the terminal for the duration of the work
// it wraps, and the hosted read is quiet underneath it (see `hosted.ts`).
//
// A step also OWNS the line its work is reported on. `spinner.stop(text)` in
// 1.8.1 takes no status code, so a step that stopped with its own label and
// left the caller to print the outcome spent two lines on the one thing, and a
// step that stopped with the outcome printed a warning under a done marker.
// The spinner's three finishers carry the three markers exactly — `stop` is
// `◇`, `error` is `▲`, `cancel` is `■` — so the work's report goes to `step`
// and the spinner stops as whatever the work turned out to be (carrick#1032).

import readline from "node:readline/promises";
import type { Writable } from "node:stream";
import * as clack from "@clack/prompts";
import pc from "picocolors";

export const DONE = "◇";
export const WARN = "▲";
export const REFUSE = "■";

export const DOCS = "https://docs.carrick.tools/quickstart";

/**
 * What a step's work turned out to be: the line, and the marker it carries.
 *
 * The three kinds are the three markers, and they are the same three
 * [`InitOutput`] prints by hand — a step is one of these lines, produced by
 * work rather than by a decision already made.
 */
export type StepReport = { kind: "done" | "warn" | "refuse"; text: string };

/** Everything `init` prints or asks. */
export type InitOutput = {
  /** A thing this run did. */
  done(text: string): void;
  /** Done, but something needs attention: the text says what to do. */
  warn(text: string): void;
  /** Not done: the text says why. */
  refuse(text: string): void;
  /** A line with no state of its own — a browser step, a list, a prompt. */
  say(text: string): void;
  /** The closing block: a title and the lines under it. */
  note(title: string, body: string[]): void;
  /**
   * Run `work`, showing progress while it runs, and report what it did.
   *
   * One line for the whole step, whichever rendering this is: `report` turns
   * the work's value into the line and the marker, and a spinner stops as that
   * marker rather than repeating the label it started with (carrick#1032).
   */
  step<T>(label: string, work: () => Promise<T>, report: (value: T) => StepReport): Promise<T>;
  /** Yes or no, defaulting to yes, as the old readline prompt did. */
  confirm(question: string): Promise<boolean>;
  /** A typed answer, for the questions whose answer is not yes or no. */
  ask(question: string): Promise<string>;
  /** Dim a fragment, where this rendering has colour to dim it with. */
  accent(text: string): string;
  /** Whether the work under `step` may write to the terminal itself. */
  readonly quiet: boolean;
};

/** Plain text: the same lines, no spinner, no colour, no gutter. */
export function plainOutput(write: (text: string) => void = (text) => process.stdout.write(text)): InitOutput {
  const line = (marker: string, text: string): void => {
    for (const entry of text.split("\n")) write(`${marker} ${entry}\n`);
  };
  const marker = (kind: StepReport["kind"]): string =>
    kind === "done" ? DONE : kind === "warn" ? WARN : REFUSE;
  return {
    done: (text) => line(DONE, text),
    warn: (text) => line(WARN, text),
    refuse: (text) => line(REFUSE, text),
    say: (text) => write(`${text}\n`),
    note: (title, body) => {
      write("\n");
      write(`${title}\n`);
      for (const entry of body) write(`  ${entry}\n`);
      write("\n");
    },
    // No spinner here, so the label was never printed: the step is exactly the
    // one line its work reports, which is what the plain rendering already
    // spent on it.
    step: async (_label, work, report) => {
      const value = await work();
      const outcome = report(value);
      line(marker(outcome.kind), outcome.text);
      return value;
    },
    // No colour, ever. picocolors turns itself ON when `CI` is set, which is
    // exactly the run whose output is captured as text, so the decision is made
    // here from the terminal rather than by the library from the environment.
    accent: (text) => text,
    // No terminal to ask with. `init` refuses before this can be reached
    // without `--yes`; a stdin that is a TTY under a piped stdout still gets a
    // real question rather than a silent yes.
    confirm: async (question) => {
      const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
      try {
        const answer = await rl.question(`${question} [Y/n] `);
        return answer.trim() === "" || /^y(es)?$/i.test(answer.trim());
      } finally {
        rl.close();
      }
    },
    ask: async (question) => {
      const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
      try {
        return (await rl.question(`${question}\n> `)).trim();
      } finally {
        rl.close();
      }
    },
    quiet: false,
  };
}

/**
 * clack's rendering, for a terminal.
 *
 * `output` is where the rendering goes. It is threaded into every clack call
 * rather than left to the library's default because that is the only way this
 * rendering can be read back: a terminal's own bytes are not capturable, and
 * the plain one — which every executable test spawns — is a different renderer
 * with different lines. A test that hands this a stream is the only thing that
 * sees the interactive markers at all (carrick#1032).
 */
export function interactiveOutput(output: Writable = process.stdout): InitOutput {
  return {
    done: (text) => clack.log.step(text, { output }),
    warn: (text) => clack.log.warn(text, { output }),
    refuse: (text) => clack.log.error(text, { output }),
    say: (text) => clack.log.message(text, { output }),
    note: (title, body) => clack.note(body.join("\n"), title, { output }),
    // The spinner's finishers ARE the markers: `stop` prints `◇`, `error`
    // prints `▲` and `cancel` prints `■`, so the step ends on one line with
    // the marker its work earned. Stopping with the label instead cost the
    // hosted read two lines, and stopping with the outcome text alone put
    // warnings and refusals under a done marker (carrick#1032).
    step: async (label, work, report) => {
      const spinner = clack.spinner({ output });
      spinner.start(label);
      let value: Awaited<ReturnType<typeof work>>;
      try {
        value = await work();
      } catch (error) {
        // The step did not finish, so it is a refusal: the caller prints why.
        spinner.cancel(label);
        throw error;
      }
      const outcome = report(value);
      if (outcome.kind === "done") spinner.stop(outcome.text);
      else if (outcome.kind === "warn") spinner.error(outcome.text);
      else spinner.cancel(outcome.text);
      return value;
    },
    // A cancelled prompt is a no: `init` returns 0 and writes nothing, which
    // is what answering "n" has always done.
    confirm: async (question) => {
      const answer = await clack.confirm({ message: question, initialValue: true, output });
      return clack.isCancel(answer) ? false : answer;
    },
    ask: async (question) => {
      const answer = await clack.text({ message: question, output });
      return clack.isCancel(answer) ? "" : answer.trim();
    },
    accent: (text) => pc.dim(text),
    quiet: true,
  };
}

/**
 * The rendering this terminal gets.
 *
 * stdout decides, not stdin: a run whose output is piped or captured is read
 * as text by whatever holds the other end, and a spinner's rewrites and box
 * art are noise there even when a terminal is still attached to the input.
 *
 * A width of zero is the second half of the test, and it is not hypothetical: a
 * pty with no window size — which is what `script` and several agent harnesses
 * hand a child — reports `isTTY` true and `columns` 0, and clack then wraps its
 * box to one character per line. Measured on macOS, 2026-09-13 (carrick#1026).
 */
export function createOutput(
  tty: boolean = process.stdout.isTTY === true && (process.stdout.columns ?? 0) > 0,
): InitOutput {
  return tty ? interactiveOutput() : plainOutput();
}
