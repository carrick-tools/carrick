import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export const API_BASE = "https://api.carrick.tools";
export const APP_BASE = "https://app.carrick.tools";

/** Shared with the native read client. Tokens expire by server revocation. */
export type Credential = {
  api_base: string;
  token: string;
  workspace_slug: string | null;
  obtained_at: string;
};

export function credentialPath(env: NodeJS.ProcessEnv = process.env): string {
  const base = env["XDG_CONFIG_HOME"] ||
    (process.platform === "win32" ? env["APPDATA"] : undefined) ||
    path.join(env["HOME"] || os.homedir(), ".config");
  if (!path.isAbsolute(base)) throw new Error("Carrick's configuration directory must be an absolute path.");
  return path.join(base, "carrick", "credentials.json");
}

export function readCredential(env: NodeJS.ProcessEnv = process.env): Credential | null {
  if (env["CARRICK_TOKEN"] !== undefined) {
    const token = env["CARRICK_TOKEN"];
    if (!token || /\s/.test(token)) throw new Error("CARRICK_TOKEN is empty or malformed. Set it to a Carrick token or unset it and run carrick login.");
    return { api_base: API_BASE, token, workspace_slug: null, obtained_at: "" };
  }
  const file = credentialPath(env);
  let fd: number;
  try { fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw new Error("Could not read Carrick credentials. Run carrick login.");
  }
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile() || (process.platform !== "win32" && (stat.mode & 0o077) !== 0)) throw new Error();
    const value: unknown = JSON.parse(fs.readFileSync(fd, "utf8"));
    if (typeof value !== "object" || value === null) throw new Error();
    const credential = value as Credential;
    if (credential.api_base !== API_BASE || typeof credential.token !== "string" ||
        !credential.token || /\s/.test(credential.token) ||
        !(credential.workspace_slug === null || typeof credential.workspace_slug === "string") ||
        typeof credential.obtained_at !== "string") throw new Error();
    return credential;
  } catch { throw new Error("Carrick credentials are invalid or not private. Run carrick login."); }
  finally { fs.closeSync(fd); }
}

export function saveCredential(token: string, workspace: string | null, env: NodeJS.ProcessEnv = process.env): void {
  if (!token || /\s/.test(token)) throw new Error("The authorization server returned an invalid token.");
  const file = credentialPath(env);
  const directory = path.dirname(file);
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  if (!fs.lstatSync(directory).isDirectory()) throw new Error("Carrick's credential directory must not be a symbolic link.");
  if (process.platform !== "win32") fs.chmodSync(directory, 0o700);
  const temporary = fs.mkdtempSync(path.join(directory, ".login-"));
  try {
    const pending = path.join(temporary, "credentials.json");
    const credential: Credential = { api_base: API_BASE, token, workspace_slug: workspace, obtained_at: new Date().toISOString() };
    fs.writeFileSync(pending, `${JSON.stringify(credential, null, 2)}\n`, { mode: 0o600, flag: "wx" });
    fs.renameSync(pending, file);
  } finally { fs.rmSync(temporary, { recursive: true, force: true }); }
}

/** Removes the local credential only; dashboard revocation is a separate action. */
export function removeCredential(env: NodeJS.ProcessEnv = process.env): boolean {
  try { fs.unlinkSync(credentialPath(env)); return true; }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return false;
    throw new Error("Could not remove Carrick's credential file.");
  }
}
