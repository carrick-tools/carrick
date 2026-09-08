// The GitHub identity `carrick init` requires.
//
// There is no anonymous tier (carrick#729, ruled 2026-09-08): the local index
// is built from the identified free tier's allowance, so setup asks who you are
// before it writes anything. The check is offline and reuses whatever the
// machine already has — the GitHub CLI's login, or a token in the environment —
// rather than opening a browser flow of its own.
//
// This states the identity. Spending an allowance against it is the cloud's
// side (carrick-cloud#601) and is not wired here.

import { spawnSync } from "node:child_process";

export type Identity = {
  /** The GitHub login, when the source could name one. */
  login: string | null;
  source: "gh" | "token";
};

export type IdentityLookup = {
  identity: Identity | null;
  /** What to do about it, ready to print. Null when there is an identity. */
  problem: string | null;
};

export type IdentityOptions = {
  env?: NodeJS.ProcessEnv;
  /** Injectable for tests: runs `gh auth status` and returns its output. */
  ghStatus?: () => string;
};

function defaultGhStatus(): string {
  // Both streams: `gh auth status` writes to stderr on older versions and to
  // stdout on newer ones, and a run that reads one of them finds nothing on
  // half the machines it runs on.
  const run = spawnSync("gh", ["auth", "status"], { encoding: "utf8" });
  if (run.error) throw run.error;
  return `${run.stdout ?? ""}\n${run.stderr ?? ""}`;
}

/** The login out of `gh auth status`, whatever wording it used. */
export function loginFromGhStatus(output: string): string | null {
  const match =
    /Logged in to \S+ account (\S+)/.exec(output) ?? /Logged in to \S+ as (\S+)/.exec(output);
  return match?.[1] ?? null;
}

export function githubIdentity(options: IdentityOptions = {}): IdentityLookup {
  const env = options.env ?? process.env;
  const ghStatus = options.ghStatus ?? defaultGhStatus;
  try {
    const login = loginFromGhStatus(ghStatus());
    if (login) return { identity: { login, source: "gh" }, problem: null };
  } catch {
    // No gh, or gh is not logged in. The token below is the other way in.
  }

  const token = env["GITHUB_TOKEN"] || env["GH_TOKEN"];
  if (token) return { identity: { login: null, source: "token" }, problem: null };

  return {
    identity: null,
    problem: [
      "carrick init needs your GitHub identity, and this machine has none it can use.",
      "",
      "Either:",
      "  gh auth login                 (the GitHub CLI: https://cli.github.com)",
      "  export GITHUB_TOKEN=<token>   (any token that identifies you)",
      "",
      "Carrick indexes what you already have access to, and the free tier's",
      "allowance is counted against the account that asks for it.",
    ].join("\n"),
  };
}

/** One line for the summary. */
export function describeIdentity(identity: Identity): string {
  return identity.login
    ? `GitHub identity: ${identity.login} (from the GitHub CLI)`
    : "GitHub identity: the token in this environment";
}
