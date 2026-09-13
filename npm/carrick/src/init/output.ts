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

import readline from "node:readline/promises";
import * as clack from "@clack/prompts";
import pc from "picocolors";

export const DONE = "◇";
export const WARN = "▲";
export const REFUSE = "■";

export const DOCS = "https://docs.carrick.tools/quickstart";

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
  /** Run `work`, showing progress while it runs. */
  step<T>(label: string, work: () => Promise<T>): Promise<T>;
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
    step: async (_label, work) => await work(),
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

/** clack's rendering, for a terminal. */
export function interactiveOutput(): InitOutput {
  return {
    done: (text) => clack.log.step(text),
    warn: (text) => clack.log.warn(text),
    refuse: (text) => clack.log.error(text),
    say: (text) => clack.log.message(text),
    note: (title, body) => clack.note(body.join("\n"), title),
    step: async (label, work) => {
      const spinner = clack.spinner();
      spinner.start(label);
      try {
        const value = await work();
        spinner.stop(label);
        return value;
      } catch (error) {
        spinner.stop(label);
        throw error;
      }
    },
    // A cancelled prompt is a no: `init` returns 0 and writes nothing, which
    // is what answering "n" has always done.
    confirm: async (question) => {
      const answer = await clack.confirm({ message: question, initialValue: true });
      return clack.isCancel(answer) ? false : answer;
    },
    ask: async (question) => {
      const answer = await clack.text({ message: question });
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
 */
export function createOutput(tty: boolean = process.stdout.isTTY === true): InitOutput {
  return tty ? interactiveOutput() : plainOutput();
}
