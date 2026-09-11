import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import http from "node:http";
import { spawn } from "node:child_process";
import { APP_BASE, API_BASE, SCOPE } from "./credentials.ts";

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
  const server = http.createServer((req, res) => {
    res.setHeader("Cache-Control", "no-store");
    res.setHeader("Content-Type", "text/plain; charset=utf-8");
    let url: URL;
    try { url = new URL(req.url ?? "/", "http://127.0.0.1"); }
    catch { res.writeHead(400).end("Invalid authorization callback."); return; }
    const supplied = url.searchParams.get("state") ?? "";
    if (consumed || req.method !== "GET" || url.pathname !== "/callback" ||
        url.searchParams.getAll("state").length !== 1 ||
        Buffer.byteLength(supplied) !== Buffer.byteLength(state) ||
        !timingSafeEqual(Buffer.from(supplied), Buffer.from(state))) {
      res.writeHead(400).end("Invalid authorization callback.");
      return;
    }
    if (url.searchParams.has("error")) {
      consumed = true;
      res.writeHead(400).end("Carrick authorization was declined. Return to your terminal.");
      settle(new Error("Carrick authorization was declined. Run carrick login to try again."));
      return;
    }
    const code = url.searchParams.get("code");
    if (!code || url.searchParams.getAll("code").length !== 1) {
      res.writeHead(400).end("Missing authorization code.");
      return;
    }
    consumed = true;
    res.end("Authorization received. Return to your terminal to finish Carrick login.");
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
    return body.access_token;
  } catch (error) {
    if (signal.aborted) throw new Error("Carrick login was cancelled or timed out. Run carrick login to try again.");
    // Never include a remote body, token, verifier or callback URL in an error.
    if (error instanceof Error && error.message.startsWith("Carrick")) throw error;
    throw new Error("Carrick login could not complete. Check the connection and loopback listener, then run carrick login.");
  } finally {
    signal.removeEventListener("abort", cancel);
    server.closeAllConnections();
    if (server.listening) await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}
