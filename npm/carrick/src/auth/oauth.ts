import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import http from "node:http";
import { spawn } from "node:child_process";
import { APP_BASE, API_BASE, SCOPE } from "./credentials.ts";
import { resolveRepos } from "./read.ts";

/** Launch a URL as an argument, never through a shell. */
export async function openBrowser(url: string): Promise<boolean> {
  const command = process.platform === "darwin" ? "open" : process.platform === "win32" ? "rundll32.exe" : "xdg-open";
  const args = process.platform === "win32" ? ["url.dll,FileProtocolHandler", url] : [url];
  return await new Promise((resolve) => {
    const child = spawn(command, args, { stdio: "ignore" });
    child.on("error", () => resolve(false));
    child.on("exit", (code) => resolve(code === 0));
  });
}

export type OAuthOptions = {
  fetch?: typeof fetch;
  open?: (url: string) => Promise<boolean>;
  say?: (message: string) => void;
  timeoutMs?: number;
  signal?: AbortSignal;
};

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

/**
 * The page the browser lands on after the loopback redirect (carrick#1488).
 * Self-contained: inline CSS, no external assets. Kept deliberately plain:
 * the word "carrick", one heading, one line. Every argument is escaped here.
 */
export function callbackPage(heading: string, line: string): string {
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Carrick</title>
<style>
body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;background:#0b0d10;color:#e6e8eb;font:16px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:32rem;padding:2rem}
.brand{font-weight:600;letter-spacing:.02em;color:#9aa3ad;margin:0 0 1.5rem}
h1{font-size:1.5rem;font-weight:600;margin:0 0 .5rem}
p{margin:0;color:#b8c0c8}
code{font:.95em ui-monospace,SFMono-Regular,Menlo,monospace;color:#e6e8eb}
</style></head>
<body><main><p class="brand">carrick</p><h1>${escapeHtml(heading)}</h1><p>${escapeHtml(line).replace(/`([^`]+)`/g, "<code>$1</code>")}</p></main></body></html>
`;
}

function answer(res: http.ServerResponse, status: number, heading: string, line: string): void {
  if (res.writableEnded) return;
  res.writeHead(status, { "Content-Type": "text/html; charset=utf-8" }).end(callbackPage(heading, line));
}

const TRY_AGAIN = "Run `carrick login` again.";

/** RFC 8252 loopback + S256 PKCE against Carrick's existing public OAuth client flow. */
export async function authorize(options: OAuthOptions = {}): Promise<string> {
  const request = options.fetch ?? fetch;
  const say = options.say ?? ((message: string) => process.stdout.write(`${message}\n`));
  const signal = AbortSignal.any([AbortSignal.timeout(options.timeoutMs ?? 5 * 60_000), ...(options.signal ? [options.signal] : [])]);
  const verifier = randomBytes(32).toString("base64url");
  const state = randomBytes(32).toString("base64url");
  let settle: (value: string | Error) => void;
  const received = new Promise<string | Error>((resolve) => { settle = resolve; });
  let consumed = false;
  // The browser that delivered the code waits on this response until the
  // exchange has finished, so the page states the real outcome.
  let pending: http.ServerResponse | null = null;
  const server = http.createServer((req, res) => {
    res.setHeader("Cache-Control", "no-store");
    let url: URL;
    try { url = new URL(req.url ?? "/", "http://127.0.0.1"); }
    catch { answer(res, 400, "Sign-in failed", `This is not a valid sign-in link. ${TRY_AGAIN}`); return; }
    const supplied = url.searchParams.get("state") ?? "";
    if (consumed || req.method !== "GET" || url.pathname !== "/callback" ||
        url.searchParams.getAll("state").length !== 1 ||
        Buffer.byteLength(supplied) !== Buffer.byteLength(state) ||
        !timingSafeEqual(Buffer.from(supplied), Buffer.from(state))) {
      answer(res, 400, "Sign-in failed", `This is not a valid sign-in link. ${TRY_AGAIN}`);
      return;
    }
    if (url.searchParams.has("error")) {
      consumed = true;
      answer(res, 400, "Sign-in declined", `Carrick authorization was declined. ${TRY_AGAIN}`);
      settle(new Error("Carrick authorization was declined. Run carrick login to try again."));
      return;
    }
    const code = url.searchParams.get("code");
    if (!code || url.searchParams.getAll("code").length !== 1) {
      answer(res, 400, "Sign-in failed", `The sign-in link carried no authorization code. ${TRY_AGAIN}`);
      return;
    }
    consumed = true;
    pending = res;
    settle(code);
  });
  server.requestTimeout = 10_000;
  const cancel = (): void => { settle(new Error("Carrick login was cancelled or timed out. Run carrick login to try again.")); };
  signal.addEventListener("abort", cancel, { once: true });
  try {
    signal.throwIfAborted();
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", resolve);
    });
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Could not open the Carrick login callback.");
    const redirect = `http://127.0.0.1:${address.port}/callback`;
    const registration = await request(`${APP_BASE}/oauth/register`, {
      method: "POST", redirect: "error", signal,
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ client_name: "Carrick CLI", redirect_uris: [redirect], token_endpoint_auth_method: "none", grant_types: ["authorization_code"], response_types: ["code"], scope: SCOPE }),
    });
    if (!registration.ok) throw new Error(`Carrick client registration failed (HTTP ${registration.status}).`);
    const registered = await registration.json() as { client_id?: unknown };
    if (typeof registered.client_id !== "string" || !registered.client_id) throw new Error("Carrick registration returned no client ID.");
    const url = new URL(`${APP_BASE}/oauth/authorize`);
    url.search = new URLSearchParams({ response_type: "code", client_id: registered.client_id, redirect_uri: redirect, scope: SCOPE, resource: `${API_BASE}/mcp`, state, code_challenge: createHash("sha256").update(verifier).digest("base64url"), code_challenge_method: "S256" }).toString();
    say(`Sign in to Carrick in your browser:\n${url}`);
    // Opening a browser is best effort. A manual browser can finish the same callback.
    void (options.open ?? openBrowser)(url.toString()).catch(() => false);
    const code = await received;
    if (code instanceof Error) throw code;
    const result = await request(`${APP_BASE}/oauth/token`, {
      method: "POST", redirect: "error", signal,
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({ grant_type: "authorization_code", client_id: registered.client_id, redirect_uri: redirect, code, code_verifier: verifier, resource: `${API_BASE}/mcp` }),
    });
    if (!result.ok) throw new Error(`Carrick token exchange failed (HTTP ${result.status}). Run carrick login again.`);
    const body = await result.json() as { access_token?: unknown; token_type?: unknown; scope?: unknown };
    // The scope must come back as the one that was consented to. A server that
    // answers with a different one has not minted the kind this credential
    // needs, and keeping the token would leave a credential on disk that fails
    // at the first upload with no way to say why.
    if (typeof body.access_token !== "string" || !body.access_token || /\s/.test(body.access_token) ||
        typeof body.token_type !== "string" || body.token_type.toLowerCase() !== "bearer" || body.scope !== SCOPE) {
      throw new Error("Carrick returned an invalid OAuth token response.");
    }
    if (pending) {
      // The workspace name is for the page only. A failed lookup must not
      // lose an issued token: the caller saves it and reports the lookup.
      const workspace = await resolveRepos(body.access_token, [], request, signal).then((r) => r.workspace.slug, () => null);
      answer(pending, 200, workspace ? `Signed in to ${workspace}` : "Signed in to Carrick",
        "You can close this tab. Next, run `carrick init` in the folder that holds your repos.");
    }
    return body.access_token;
  } catch (error) {
    // Never include a remote body, token, verifier or callback URL in an error.
    const failure = signal.aborted
      ? new Error("Carrick login was cancelled or timed out. Run carrick login to try again.")
      : error instanceof Error && error.message.startsWith("Carrick")
        ? error
        : new Error("Carrick login could not complete. Check the connection and loopback listener, then run carrick login.");
    if (pending) {
      const said = failure.message.replace(/ Run carrick login.*$/, "").replace(/, then run carrick login\.$/, ".");
      answer(pending, 400, "Sign-in failed", `${said} ${TRY_AGAIN}`);
    }
    throw failure;
  } finally {
    signal.removeEventListener("abort", cancel);
    server.closeAllConnections();
    if (server.listening) await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}
