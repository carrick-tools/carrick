import { readCredential, saveCredential, removeCredential } from "./credentials.ts";
import { authorize } from "./oauth.ts";
import { resolveRepos } from "./read.ts";

export async function login(argv: string[]): Promise<number> {
  if (argv.length) {
    process.stdout.write("carrick login\n\nSign in through your browser. CARRICK_TOKEN overrides the saved credential.\n");
    return argv.length === 1 && ["--help", "-h"].includes(argv[0]!) ? 0 : 2;
  }
  const controller = new AbortController();
  const cancel = (): void => controller.abort();
  process.once("SIGINT", cancel);
  try {
    // An explicit override must be verified, never silently replaced by a browser login.
    const override = process.env["CARRICK_TOKEN"] !== undefined ? readCredential() : null;
    const token = override?.token ?? await authorize({ signal: controller.signal });
    // Keep an issued token if the new metadata endpoint is temporarily unavailable.
    if (!override) saveCredential(token, null);
    const resolved = await resolveRepos(token, []);
    if (!override) saveCredential(token, resolved.workspace.slug);
    process.stdout.write(`Signed in to Carrick workspace ${resolved.workspace.slug}${override ? " using CARRICK_TOKEN" : ""}.\n`);
    return 0;
  } catch (error) {
    process.stderr.write(`${(error as Error).message}\n`);
    return 1;
  } finally { process.removeListener("SIGINT", cancel); }
}

export function logout(argv: string[]): number {
  if (argv.length) {
    process.stdout.write("carrick logout\n\nRemove the saved local credential. Revoke the key at https://app.carrick.tools/account.\n");
    return argv.length === 1 && ["--help", "-h"].includes(argv[0]!) ? 0 : 2;
  }
  try {
    const removed = removeCredential();
    process.stdout.write(removed ? "Removed the saved Carrick credential.\n" : "No saved Carrick credential.\n");
    if (process.env["CARRICK_TOKEN"] !== undefined) process.stdout.write("CARRICK_TOKEN still overrides login; unset it in your shell to sign out.\n");
    process.stdout.write("To revoke the key on the server, visit https://app.carrick.tools/account.\n");
    return 0;
  } catch (error) { process.stderr.write(`${(error as Error).message}\n`); return 1; }
}
