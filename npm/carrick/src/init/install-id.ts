// What this machine is called when it queries the index.
//
// The hosted MCP server is stateless: nothing joins two tool calls arriving
// from the same laptop, so "the index felt slow" cannot be told apart from
// "that task made twenty-five searches" (carrick-cloud#890). One opaque
// per-machine id, sent as a request header on every MCP call, is the smallest
// thing that answers the question, and the header is configured once — by
// `carrick init`, into the client's own MCP entry — rather than by anything
// the agent has to remember.
//
// What it is: a UUID v4, generated here, stored in one 0600 file, and read
// back forever after. What it is NOT, and must never become: anything derived
// from the hostname, the user name or a MAC address. A derived id would
// identify the person rather than the install, would survive a deliberate
// reset, and would be the same value on two machines that happen to share a
// name. `carrick remove` deletes the file, and the next `carrick init` is a
// new install — which is the only reset anybody should have to know about.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";

/** The request header the cloud reads this as, and logs as `install_id`. */
export const INSTALL_ID_HEADER = "X-Carrick-Install-Id";

/** What the server accepts, character for character (carrick-cloud#890). */
export const INSTALL_ID_PATTERN = /^[A-Za-z0-9_-]{8,64}$/;

/**
 * Where the id lives.
 *
 * `~/.carrick`, not the configuration directory the credential uses: this is
 * not a secret and not per-workspace — it is one line naming one machine, and
 * `carrick remove` has one place to delete it from.
 */
export function installIdPath(home: string = os.homedir()): string {
  return path.join(home, ".carrick", "install-id");
}

/** The id this machine already has, or null: never creates one. */
export function readInstallId(home?: string): string | null {
  let body: string;
  try {
    body = fs.readFileSync(installIdPath(home), "utf8");
  } catch {
    return null;
  }
  const id = body.trim();
  // A file that says something the server would reject is not an id. It is
  // replaced rather than reported, because nobody edits this file on purpose.
  return INSTALL_ID_PATTERN.test(id) ? id : null;
}

/** The id this machine has, creating one the first time. Throws on a home directory that will not take a file. */
export function ensureInstallId(home?: string): string {
  const existing = readInstallId(home);
  if (existing !== null) return existing;

  const file = installIdPath(home);
  const id = randomUUID();
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  try {
    // Exclusive: two carrick processes starting at once must end with one id,
    // and the loser reads the winner's rather than overwriting it.
    fs.writeFileSync(file, `${id}\n`, { mode: 0o600, flag: "wx" });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
    const written = readInstallId(home);
    if (written !== null) return written;
    // It exists and says nothing an id can be read out of: replace it.
    fs.writeFileSync(file, `${id}\n`, { mode: 0o600 });
  }
  // `wx` honours the mode only when it creates the file, and an id written
  // over a corrupt one has whatever mode that one had.
  if (process.platform !== "win32") fs.chmodSync(file, 0o600);
  return id;
}

/**
 * The id, or null when this machine's home directory would not take one.
 *
 * The callers are the two that must not fail over this: `connectMcpClients`,
 * which may not throw at all, and the line `init` prints when it found no
 * client to configure. A missing id costs the header, never the setup.
 */
export function installIdOrNull(home?: string): string | null {
  try {
    return ensureInstallId(home);
  } catch {
    return null;
  }
}

/**
 * Delete it, as `carrick remove` does. True when there was one.
 *
 * The directory goes too when the id was the last thing in it: `~/.carrick` is
 * this package's, and an empty one left behind is litter a user would have to
 * recognise before deleting.
 */
export function removeInstallId(home?: string): boolean {
  const file = installIdPath(home);
  try {
    fs.unlinkSync(file);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return false;
    throw new Error(`Could not remove ${file}: ${(error as Error).message}`);
  }
  try {
    fs.rmdirSync(path.dirname(file));
  } catch {
    // Not empty, or not ours to remove: the id is gone either way.
  }
  return true;
}
