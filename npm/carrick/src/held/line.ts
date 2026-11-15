// The held compiler's line (part 3, D13): one break the daemon found, in words.
// The compiler's message names the check's own alias and may print whole inline
// types; both are replaced here, and a message that still names the check's
// machinery is never printed.

import { createHash } from "node:crypto";
import type { HeldLine } from "./daemon.ts";

/** At most this many characters in one line. */
export const MAX_HELD_LINE = 300;
/** A quoted inline object type longer than this becomes `the reply`. */
const MAX_INLINE_TYPE = 80;
/** The check's alias in a message: `O<row>_<group>`. */
const ALIAS = /'O\d+_\d+'/g;
/** A quoted inline object type; its inner text has no quote of its own (the compiler quotes literals with double quotes). */
const INLINE_OBJECT = /'(\{[^']*\})'/g;
/** What must never reach an agent. */
const MACHINERY = /__C\b|\breg\d|\bO\d+_\d+\b|__carrick_/;

export type RenderedHeld = {
  text: string;
  /** Once per session: the row and group, and a hash of the message. */
  claim: string;
};

/** The line for one break, or null when its message still shows the check's machinery. */
export function renderHeldLine(line: HeldLine): RenderedHeld | null {
  const message = line.message
    .replace(ALIAS, line.symbol ? `'${line.symbol}'` : "the type it expects")
    .replace(INLINE_OBJECT, (whole, inner: string) => (inner.length > MAX_INLINE_TYPE ? "the reply" : whole))
    .replace(/\s+/g, " ")
    .trim();
  if (MACHINERY.test(message)) return null;
  let text = `Carrick: this edit breaks ${line.label} for ${line.consumer} (${line.file}:${line.line}): ${message}`;
  if (text.length > MAX_HELD_LINE) text = `${text.slice(0, MAX_HELD_LINE - 1)}…`;
  const hash = createHash("sha256").update(line.message).digest("hex").slice(0, 8);
  return { text, claim: `held::${line.row_key}::${hash}` };
}
