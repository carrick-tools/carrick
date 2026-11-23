// `carrick telemetry on|off|status`: the one switch for the daily usage counts
// (`hook/usage.ts`). One person, one machine, one answer: the setting lives in
// `~/.carrick/telemetry`, not in a workspace.

import fs from "node:fs";
import { initialisedRoot } from "./outdated.ts";
import { envSwitch, lastBodyPath, markNoticed, readTelemetry, writeTelemetry } from "../hook/usage.ts";

const USAGE_LINE = "usage: carrick telemetry on|off|status\n";

export function telemetry(
  argv: string[],
  options: { env?: NodeJS.ProcessEnv; home?: string; cwd?: string; write?: (text: string) => void } = {},
): number {
  const env = options.env ?? process.env;
  const write = options.write ?? ((text: string) => void process.stdout.write(text));
  const [action, ...extra] = argv;
  if (action === "on" || action === "off") {
    if (extra.length > 0) {
      process.stderr.write(`carrick telemetry ${action} takes no arguments\n`);
      return 2;
    }
    try {
      const held = readTelemetry(options.home);
      writeTelemetry({ state: action, noticed_at: held.noticed_at }, options.home);
      if (action === "on") markNoticed(Date.now(), options.home);
    } catch (error) {
      process.stderr.write(`carrick telemetry: could not save the setting: ${(error as Error).message}\n`);
      return 1;
    }
    write(action === "on" ? "Usage counts are on. Run `carrick telemetry status` to see what is sent.\n" : "Usage counts are off. Nothing is sent.\n");
    return 0;
  }
  if (action === "status" && extra.length === 0) {
    const held = readTelemetry(options.home);
    const lines: string[] = [];
    if (held.state === "off") lines.push("Usage counts are off.");
    else if (held.noticed_at === null) lines.push("Usage counts are on, but counting starts only after the notice has been shown once. Run `carrick telemetry on` to start now.");
    else lines.push("Usage counts are on. Daily hook-run counts are sent, never code, file names, paths or repository names.");
    const forced = envSwitch(env);
    if (forced !== null) lines.push(`${forced} is set, so nothing is sent.`);
    if (env["CI"]) lines.push("CI is set, so nothing is sent.");
    const root = initialisedRoot(options.cwd ?? process.cwd());
    let last: string | null = null;
    if (root) {
      try {
        last = fs.readFileSync(lastBodyPath(root), "utf8").trimEnd();
      } catch {
        last = null;
      }
    }
    lines.push(last === null ? "Nothing has been sent from this folder yet." : `Last body sent from this folder:\n${last}`);
    write(`${lines.join("\n")}\n`);
    return 0;
  }
  process.stderr.write(USAGE_LINE);
  return 2;
}
