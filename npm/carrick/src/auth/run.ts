import { API_BASE, APP_BASE, readCredential, saveCredential, removeCredential, type Credential } from "./credentials.ts";
import { authorize } from "./oauth.ts";
import { resolveRepos } from "./read.ts";
import { readInstallId } from "../init/install-id.ts";

/**
 * Sign in through the browser and leave a saved credential behind.
 *
 * Shared with `carrick init`, which signs a machine in rather than stopping to
 * say "run carrick login" (carrick#955): the first thing a new install does is
 * the one thing it was refusing to do. The steps are the same either way, so
 * there is one copy of them.
 */
export async function signIn(
  say: (message: string) => void = (message) => process.stdout.write(`${message}\n`),
  signal?: AbortSignal,
): Promise<Credential> {
  // An explicit override must be verified, never silently replaced by a browser login.
  const override = process.env["CARRICK_TOKEN"] !== undefined ? readCredential() : null;
  const token = override?.token ?? await authorize(signal ? { signal } : {});
  // Keep an issued token if the new metadata endpoint is temporarily unavailable.
  if (!override) saveCredential(token, null);
  const resolved = await resolveRepos(token, []);
  if (!override) saveCredential(token, resolved.workspace.slug);
  say(`Signed in to Carrick workspace ${resolved.workspace.slug}${override ? " using CARRICK_TOKEN" : ""}.`);
  return (
    override ?? {
      api_base: API_BASE,
      token,
      workspace_slug: resolved.workspace.slug,
      obtained_at: new Date().toISOString(),
    }
  );
}

/**
 * What a `carrick login` says to do next, or null when there is nothing.
 *
 * The browser page used to say "Next, run carrick init" to everyone, which was
 * wrong under init itself and on every login after the first (carrick#1511).
 * The terminal knows which it is: `carrick init` writes this machine's install
 * id and `carrick remove` deletes it, so a machine without one has never been
 * set up, and that is the only login with a step left.
 */
export function loginNextStep(installId: string | null = readInstallId()): string | null {
  return installId === null ? "Next: run carrick init in the folder that holds your repos." : null;
}

export async function login(argv: string[]): Promise<number> {
  if (argv.length) {
    process.stdout.write("carrick login\n\nSign in through your browser. CARRICK_TOKEN overrides the saved credential.\n");
    return argv.length === 1 && ["--help", "-h"].includes(argv[0]!) ? 0 : 2;
  }
  const controller = new AbortController();
  const cancel = (): void => controller.abort();
  process.once("SIGINT", cancel);
  try {
    await signIn(undefined, controller.signal);
    const next = loginNextStep();
    if (next !== null) process.stdout.write(`${next}\n`);
    return 0;
  } catch (error) {
    process.stderr.write(`${(error as Error).message}\n`);
    return 1;
  } finally { process.removeListener("SIGINT", cancel); }
}

/**
 * Ask Carrick to revoke the key that makes the request (carrick#1487).
 *
 * The key is its own authority: `POST <app>/oauth/revoke` hashes the bearer
 * and revokes that one row, so the user's other machines and editor keys
 * survive. Only a 204 means the key is no longer live (an unknown or
 * already-revoked key is a 204 too). Every other outcome is "not revoked":
 * a 200, which the endpoint never sends; the 404 an older cloud gives for a
 * route it does not have; and a network failure. The caller signs out
 * locally either way.
 */
export async function revokeKey(token: string, request: typeof fetch = fetch): Promise<boolean> {
  try {
    const result = await request(`${APP_BASE}/oauth/revoke`, {
      method: "POST", redirect: "error", signal: AbortSignal.timeout(10_000),
      headers: { Authorization: `Bearer ${token}` },
    });
    return result.status === 204;
  } catch { return false; }
}

export type LogoutOptions = {
  env?: NodeJS.ProcessEnv;
  fetch?: typeof fetch;
  out?: (text: string) => void;
  err?: (text: string) => void;
};

export async function logout(argv: string[], options: LogoutOptions = {}): Promise<number> {
  const out = options.out ?? ((text: string) => process.stdout.write(text));
  const err = options.err ?? ((text: string) => process.stderr.write(text));
  if (argv.length) {
    out("carrick logout\n\nSign this machine out: revoke its Carrick key on the server and remove the saved credential.\n");
    return argv.length === 1 && ["--help", "-h"].includes(argv[0]!) ? 0 : 2;
  }
  const env = options.env ?? process.env;
  // The saved credential, never CARRICK_TOKEN: logout signs this machine's
  // login out, and an override is a key the user manages somewhere else.
  const { CARRICK_TOKEN: _override, ...fileEnv } = env;
  let saved: Credential | null = null;
  // An unreadable file still gets removed below; there is just no key to revoke.
  try { saved = readCredential(fileEnv); } catch { saved = null; }
  try {
    const revoked = saved ? await revokeKey(saved.token, options.fetch) : false;
    const removed = removeCredential(env);
    const where = saved?.workspace_slug ?? "Carrick";
    if (!removed) out("No saved Carrick credential.\n");
    else if (revoked) out(`Signed out of ${where}.\n`);
    else out(`Signed out of ${where} on this machine, but Carrick could not revoke the key on the server. Revoke it at ${APP_BASE}/account.\n`);
    if (env["CARRICK_TOKEN"] !== undefined) out("CARRICK_TOKEN still overrides login; unset it in your shell to sign out.\n");
    return 0;
  } catch (error) { err(`${(error as Error).message}\n`); return 1; }
}
