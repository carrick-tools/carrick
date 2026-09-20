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
import type { Readable, Writable } from "node:stream";
import * as clack from "@clack/prompts";
import { MultiSelectPrompt } from "@clack/core";
import pc from "picocolors";

export const DONE = "◇";
export const WARN = "▲";
export const REFUSE = "■";

export const DOCS = "https://docs.carrick.tools/quickstart";

/**
 * A question the user ended rather than answered: Ctrl-C, Escape, or an input
 * that closed under them.
 *
 * It is an error and not a value because every caller would otherwise have to
 * remember that one of its answers means "stop", and one that forgot carried
 * on with the wrong one: a cancelled project question was read as "leave it to
 * the browser" and the run continued into the steps that question governed
 * (carrick#1338). Thrown, it ends the run wherever it is asked, and `init`
 * asks everything before it writes anything.
 */
export class PromptCancelled extends Error {
  constructor() {
    super("cancelled");
    this.name = "PromptCancelled";
  }
}

/** One row of a `choose`: what it is called, and what it is worth knowing. */
export type Choice = { value: string; label: string; hint?: string };

/**
 * How a `choose` opens, and whether it may be answered with nothing.
 *
 * `initial` is the set that starts selected, and it is a parameter rather than
 * "all of them" because a default is a claim: the repos this install covers
 * are preselected where something already says so, and an editor's
 * configuration file outside the workspace is not preselected on the strength
 * of a directory (carrick#1365).
 */
export type ChoiceSet = { initial: string[]; required: boolean };

/**
 * The line that says what the keys do and where the answer stands.
 *
 * Said in words because the glyphs cannot say it: clack 1.8.1 draws the row
 * under the cursor and an unselected row with the SAME character
 * (`S_CHECKBOX_ACTIVE` and `S_CHECKBOX_INACTIVE` are both `◻`), so "the
 * highlighted one" and "the ones that are in" are told apart by fill and
 * colour alone. The picker below marks every row `[x]` or `[ ]` instead, and
 * this line carries the count (carrick#1365).
 */
export function chooseSentence(noun: string, selected: number, total: number): string {
  return `Space toggles ${noun}, Enter confirms. ${selected} of ${total} selected.`;
}

/**
 * What a step's work turned out to be: the line, and the marker it carries.
 *
 * The three kinds are the three markers, and they are the same three
 * [`InitOutput`] prints by hand — a step is one of these lines, produced by
 * work rather than by a decision already made.
 */
export type StepReport = { kind: "done" | "warn" | "refuse"; text: string };

/**
 * Say where a running step has got to, without ending it.
 *
 * Handed to the step's work rather than exposed on its own, because only the
 * step that owns the terminal may write to it: a spinner rewrites one line, so
 * anything else printing underneath corrupts it. In the plain rendering this
 * does nothing — one line per step is what a pipe gets, and a scan of a large
 * repo would otherwise write thousands (carrick#1315).
 */
export type StepProgress = (text: string) => void;

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
  /**
   * Open a run: the name and version of what is about to happen.
   *
   * `init` prints its lines without one; a rendered scan opens with it, so the
   * block a first run sees is bounded at both ends (carrick#1315).
   */
  intro(text: string): void;
  /** Close a run with the one next step. */
  outro(text: string): void;
  /** The closing block: a title and the lines under it. */
  note(title: string, body: string[]): void;
  /**
   * Run `work`, showing progress while it runs, and report what it did.
   *
   * One line for the whole step, whichever rendering this is: `report` turns
   * the work's value into the line and the marker, and a spinner stops as that
   * marker rather than repeating the label it started with (carrick#1032).
   */
  step<T>(
    label: string,
    work: (progress: StepProgress) => Promise<T>,
    report: (value: T) => StepReport,
  ): Promise<T>;
  /** Yes or no, defaulting to yes, as the old readline prompt did. */
  confirm(question: string): Promise<boolean>;
  /** A typed answer, for the questions whose answer is not yes or no. */
  ask(question: string): Promise<string>;
  /**
   * Which of these, opening on the set `config.initial` names.
   *
   * `config.required` decides whether an empty answer is one: the repos this
   * install covers must include one repo, and "no editors" is an ordinary
   * answer (carrick#1338, carrick#1365). `noun` is what a row is, for the
   * sentence that says what the keys do.
   */
  choose(question: string, noun: string, options: Choice[], config: ChoiceSet): Promise<string[]>;
  /** Dim a fragment, where this rendering has colour to dim it with. */
  accent(text: string): string;
  /** Whether the work under `step` may write to the terminal itself. */
  readonly quiet: boolean;
};

/**
 * One question on a terminal with no prompt library: the answer, or a cancel.
 *
 * A closed input is a cancel and has to be caught here, because `question`
 * never settles when the stream ends under it — the interface closes, the loop
 * drains, and a run that asked nothing further would exit 0 as though the
 * question had been answered (measured 2026-09-20, carrick#1338). `SIGINT`
 * reaches this only while a readline interface is open on a TTY; without the
 * listener Node's own default kills the process, which ends the run too.
 */
async function askOnce(input: Readable, output: Writable, text: string): Promise<string> {
  const rl = readline.createInterface({ input, output });
  const cancelled = new Promise<never>((_, reject) => {
    rl.once("SIGINT", () => {
      rl.close();
      reject(new PromptCancelled());
    });
    rl.once("close", () => reject(new PromptCancelled()));
  });
  // Every answer closes the interface too, so this rejects after the race is
  // already settled. Without a handler of its own that is an unhandled
  // rejection, which Node ends the process on.
  cancelled.catch(() => {});
  try {
    return await Promise.race([rl.question(text), cancelled]);
  } finally {
    rl.close();
  }
}

/**
 * The numbers an answer names, or null when it names none of them.
 *
 * Empty is the marked set, not "all of them": the marks are what the rows
 * above the question already state, and a reader pressing Enter is accepting
 * what they can see (carrick#1365). `all` and `none` are the two words that
 * say the whole and the empty set; `none` is refused where an empty answer is
 * not one, which is the same refusal an out-of-range number gets.
 */
export function chosenNumbers(
  answer: string,
  count: number,
  config: { initial: number[]; required: boolean },
): number[] | null {
  const trimmed = answer.trim();
  if (trimmed === "") {
    return config.required && config.initial.length === 0 ? null : config.initial;
  }
  if (/^all$/i.test(trimmed)) return Array.from({ length: count }, (_, index) => index);
  if (/^none$/i.test(trimmed)) return config.required ? null : [];
  const picked: number[] = [];
  for (const part of trimmed.split(/[\s,]+/)) {
    if (!/^\d+$/.test(part)) return null;
    const index = Number(part) - 1;
    if (index < 0 || index >= count) return null;
    if (!picked.includes(index)) picked.push(index);
  }
  return picked.length > 0 ? picked : null;
}

/** Plain text: the same lines, no spinner, no colour, no gutter. */
export function plainOutput(
  write: (text: string) => void = (text) => process.stdout.write(text),
  // The streams the questions are asked on. Named rather than taken from the
  // process so a test can state a cancel: an input it ends is the one thing
  // that proves a closed terminal stops the run rather than answering it.
  io: { input?: Readable; output?: Writable } = {},
): InitOutput {
  const input = io.input ?? process.stdin;
  const echo = io.output ?? process.stdout;
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
    intro: (text) => write(`${text}\n`),
    outro: (text) => write(`${text}\n`),
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
      const value = await work(() => {});
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
      const answer = (await askOnce(input, echo, `${question} [Y/n] `)).trim();
      return answer === "" || /^y(es)?$/i.test(answer);
    },
    ask: async (question) => (await askOnce(input, echo, `${question}\n> `)).trim(),
    // The list, then the numbers to keep. Every row is printed with its own
    // number because this rendering has no cursor to move: the question a
    // reader answers has to carry the whole choice. The `[x]`/`[ ]` marker is
    // the same one the terminal picker draws, so what an answer of Enter
    // accepts is on the screen in both renderings (carrick#1365).
    choose: async (question, noun, options, config) => {
      const initial = options
        .map((option, index) => (config.initial.includes(option.value) ? index : -1))
        .filter((index) => index >= 0);
      write(`${question}\n`);
      options.forEach((option, index) => {
        const mark = initial.includes(index) ? "[x]" : "[ ]";
        write(`  ${index + 1}. ${mark} ${option.label}${option.hint === undefined ? "" : `  (${option.hint})`}\n`);
      });
      write(`${chooseSentence(noun, initial.length, options.length)}\n`);
      const accepts = config.required && initial.length === 0 ? "" : ", or Enter for the marked set";
      const empties = config.required ? "" : `, or "none"`;
      for (let attempt = 0; attempt < 3; attempt += 1) {
        const answer = await askOnce(
          input,
          echo,
          `Numbers to include, separated by commas${empties}${accepts}\n> `,
        );
        const picked = chosenNumbers(answer, options.length, { initial, required: config.required });
        if (picked) return picked.map((index) => options[index]!.value);
        write(`${REFUSE} Not a number between 1 and ${options.length}: "${answer.trim()}"\n`);
      }
      throw new PromptCancelled();
    },
    quiet: false,
  };
}

/** The gutter clack draws its own prompts in, so this one sits in the block. */
const BAR = "│";

/**
 * The multi-select this package asks with, drawn rather than borrowed.
 *
 * `@clack/prompts`' own `multiselect` is the prompt the owner could not read:
 * its cursor glyph and its unselected glyph are the same character, so the row
 * under the cursor and the rows that are in are distinguished by fill and
 * colour. This is the same prompt object underneath — `MultiSelectPrompt` from
 * `@clack/core`, which is what `@clack/prompts` builds on, at the version it
 * pins — with a body that states each row's state as `[x]` or `[ ]`, the
 * cursor as `>`, and the count in the header (carrick#1365).
 *
 * `input` and `output` are threaded for the same reason every other prompt
 * here threads them: a terminal's own bytes are not capturable, and a test
 * that drives this one with a stream is the only thing that sees these markers
 * at all.
 */
function pickMany(
  question: string,
  noun: string,
  options: Choice[],
  config: ChoiceSet,
  output: Writable,
  input: Readable,
): Promise<string[] | symbol> {
  const prompt = new MultiSelectPrompt<Choice>({
    options,
    initialValues: options.filter((option) => config.initial.includes(option.value)).map((option) => option.value),
    required: config.required,
    output,
    input,
    render() {
      const chosen = new Set((this.value as string[] | undefined) ?? []);
      const rows = this.options.map((option, index) => {
        const mark = chosen.has(option.value) ? "[x]" : "[ ]";
        const cursor = index === this.cursor ? ">" : " ";
        const label = index === this.cursor ? pc.cyan(option.label) : option.label;
        const hint = option.hint === undefined ? "" : pc.dim(`  (${option.hint})`);
        return `${BAR} ${cursor} ${mark} ${label}${hint}`;
      });
      const header = `${BAR}  ${pc.dim(chooseSentence(noun, chosen.size, this.options.length))}`;
      if (this.state === "submit" || this.state === "cancel") {
        return `${pc.dim(question)}\n${BAR}  ${chosen.size} of ${this.options.length} selected`;
      }
      return [question, header, ...rows].join("\n");
    },
  });
  return prompt.prompt() as Promise<string[] | symbol>;
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
export function interactiveOutput(output: Writable = process.stdout, keys: Readable = process.stdin): InitOutput {
  return {
    done: (text) => clack.log.step(text, { output }),
    warn: (text) => clack.log.warn(text, { output }),
    refuse: (text) => clack.log.error(text, { output }),
    say: (text) => clack.log.message(text, { output }),
    intro: (text) => clack.intro(text, { output }),
    outro: (text) => clack.outro(text, { output }),
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
        value = await work((text) => spinner.message(text));
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
    // A cancelled prompt ends the run. It used to be read as the prompt's own
    // quiet answer — "no" for a confirm, an empty string for a question — so
    // Ctrl-C at the project question carried on into the steps that question
    // governed (carrick#1338).
    confirm: async (question) => {
      const answer = await clack.confirm({ message: question, initialValue: true, output, input: keys });
      if (clack.isCancel(answer)) throw new PromptCancelled();
      return answer;
    },
    ask: async (question) => {
      const answer = await clack.text({ message: question, output, input: keys });
      if (clack.isCancel(answer)) throw new PromptCancelled();
      return answer.trim();
    },
    choose: async (question, noun, options, config) => {
      const answer = await pickMany(question, noun, options, config, output, keys);
      if (clack.isCancel(answer) || !Array.isArray(answer)) throw new PromptCancelled();
      return answer;
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
