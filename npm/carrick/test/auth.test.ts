import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import http from "node:http";
import test from "node:test";
import { credentialPath, readCredential, saveCredential, removeCredential, API_BASE, APP_BASE, SCOPE } from "../src/auth/credentials.ts";
import { logout } from "../src/auth/run.ts";
import { authorize, callbackPage, type OAuthOptions } from "../src/auth/oauth.ts";
import { resolveRepos } from "../src/auth/read.ts";

test("credentials are private, replaced atomically, overridden only by CARRICK_TOKEN and removed locally", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-auth-"));
  const env = { XDG_CONFIG_HOME: dir };
  try {
    assert.equal(readCredential(env), null);
    assert.equal(readCredential({ ...env, GH_TOKEN: "github", GITHUB_TOKEN: "github" }), null);
    saveCredential("first", "acme", env);
    const file = credentialPath(env);
    assert.equal(readCredential(env)?.token, "first");
    if (process.platform !== "win32") assert.equal(fs.statSync(file).mode & 0o777, 0o600);
    assert.equal(readCredential({ ...env, CARRICK_TOKEN: "override" })?.token, "override");
    assert.throws(() => readCredential({ ...env, CARRICK_TOKEN: "" }), /CARRICK_TOKEN/);
    saveCredential("second", "acme", env);
    assert.equal(readCredential(env)?.token, "second");
    assert.equal(removeCredential(env), true);
    assert.equal(removeCredential(env), false);
    fs.writeFileSync(file, "{broken");
    assert.throws(() => readCredential(env), /carrick login/);
    assert.equal(readCredential({ ...env, CARRICK_TOKEN: "override" })?.token, "override");
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("a credential file cannot redirect a Bearer token to another API", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-auth-"));
  const env = { XDG_CONFIG_HOME: dir };
  try {
    saveCredential("secret", "acme", env);
    const file = credentialPath(env);
    const value = JSON.parse(fs.readFileSync(file, "utf8"));
    value.api_base = "https://example.com";
    fs.writeFileSync(file, JSON.stringify(value));
    assert.throws(() => readCredential(env), /carrick login/);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("resolve rejects an old 200 schema, reauth on 401, and does not echo server secrets", async () => {
  const reply = (body: unknown, status = 200): typeof fetch => async () => new Response(JSON.stringify(body), { status });
  await assert.rejects(resolveRepos("secret", [], reply({})), /schema/);
  await assert.rejects(resolveRepos("secret", [], reply({ error: "secret" }, 401)), /carrick login/);
  await assert.rejects(resolveRepos("secret", [], reply({ error: "secret" }, 403)), (error: Error) => /403/.test(error.message) && !error.message.includes("secret"));
  const payload = { schema: "carrick.resolve-repos/0", workspace: { slug: "acme", billing_tier: "free", installed: true }, allowance_sentence: null, repos: [], project_repos: [] };
  assert.deepEqual(await resolveRepos("secret", [], async (url, init) => {
    assert.equal(url, `${API_BASE}/types/check-or-upload`);
    assert.equal(new Headers(init?.headers).get("Authorization"), "Bearer secret");
    assert.equal(init?.redirect, "error");
    assert.deepEqual(JSON.parse(init?.body as string), { action: "resolve-repos", repos: [] });
    return new Response(JSON.stringify(payload));
  }), payload);
});

const RESOLVED = { schema: "carrick.resolve-repos/0", workspace: { slug: "acme", billing_tier: "free", installed: true }, allowance_sentence: null, repos: [], project_repos: [] };

test("loopback login binds state, S256, resource and redirect to the token exchange", async () => {
  let authorization: URL;
  let redirect = "";
  let exchanges = 0;
  const token = await authorize({
    timeoutMs: 3000,
    say: () => {},
    fetch: async (input, options) => {
      if (String(input).endsWith("/types/check-or-upload")) return Response.json(RESOLVED);
      if (String(input).endsWith("/oauth/register")) {
        const body = JSON.parse(options?.body as string);
        assert.equal(body.client_name, "Carrick CLI");
        assert.equal(body.token_endpoint_auth_method, "none");
        redirect = body.redirect_uris[0];
        assert.equal(new URL(redirect).hostname, "127.0.0.1");
        return Response.json({ client_id: "registered-client" });
      }
      exchanges++;
      const form = new URLSearchParams(options?.body as URLSearchParams);
      assert.equal(form.get("client_id"), "registered-client");
      assert.equal(form.get("redirect_uri"), redirect);
      assert.equal(form.get("code"), "accepted");
      assert.equal(form.get("resource"), `${API_BASE}/mcp`);
      assert.equal(createHash("sha256").update(form.get("code_verifier")!).digest("base64url"), authorization.searchParams.get("code_challenge"));
      return Response.json({ access_token: "issued", token_type: "Bearer", scope: SCOPE, expires_in: 31536000 });
    },
    open: async (url) => {
      authorization = new URL(url);
      assert.equal(authorization.searchParams.get("code_challenge_method"), "S256");
      assert.equal(authorization.searchParams.get("scope"), SCOPE);
      const malformed = await new Promise<number | undefined>((resolve, reject) => {
        const request = http.request({ hostname: "127.0.0.1", port: new URL(redirect).port, path: "http://[" }, (response) => {
          response.resume();
          response.on("end", () => resolve(response.statusCode));
        });
        request.on("error", reject);
        request.end();
      });
      assert.equal(malformed, 400);
      const bad = await fetch(`${redirect}?code=bad&state=wrong`);
      assert.equal(bad.status, 400);
      const accepted = await fetch(`${redirect}?code=accepted&state=${authorization.searchParams.get("state")}`);
      assert.equal(accepted.status, 200);
      return true;
    },
  });
  assert.equal(token, "issued");
  assert.equal(exchanges, 1);
  await assert.rejects(fetch(redirect));
});

/**
 * Drive one browser round trip against `authorize` and keep the page the
 * browser was shown. `callback` builds the query from the state and redirect.
 */
async function roundTrip(
  callback: (state: string) => string,
  reply: (url: string, events: string[], signal?: AbortSignal | null) => Response | Promise<Response>,
  extra: Partial<OAuthOptions> = {},
): Promise<{ outcome: PromiseSettledResult<string>; status: number; type: string | null; html: string; events: string[] }> {
  const events: string[] = [];
  let redirect = "";
  let page: Promise<Response> | undefined;
  const outcome = (await Promise.allSettled([authorize({
    timeoutMs: 3000, say: () => {}, ...extra,
    fetch: async (input, options) => {
      const url = String(input);
      if (url.endsWith("/oauth/register")) {
        redirect = JSON.parse(options?.body as string).redirect_uris[0];
        return Response.json({ client_id: "client" });
      }
      // A slow server: a page sent before the exchange would arrive first.
      await new Promise((resolve) => setTimeout(resolve, 30));
      events.push(url.endsWith("/oauth/token") ? "exchange" : "lookup");
      return reply(url, events, options?.signal);
    },
    open: async (value) => {
      page = fetch(`${redirect}?${callback(new URL(value).searchParams.get("state")!)}`)
        .then((response) => { events.push("page"); return response; });
      return true;
    },
  })]))[0]!;
  const response = await page!;
  const html = await response.text();
  return { outcome, status: response.status, type: response.headers.get("content-type"), html, events };
}

const issued = (url: string): Response => url.endsWith("/oauth/token")
  ? Response.json({ access_token: "issued", token_type: "Bearer", scope: SCOPE })
  : Response.json(RESOLVED);

test("the callback page answers after the exchange and names the workspace", async () => {
  const run = await roundTrip((state) => `code=accepted&state=${state}`, issued);
  assert.equal(run.outcome.status, "fulfilled");
  assert.equal(run.status, 200);
  assert.match(run.type ?? "", /^text\/html/);
  assert.deepEqual(run.events, ["exchange", "lookup", "page"]);
  assert.match(run.html, /<h1>Signed in to acme<\/h1>/);
  assert.match(run.html, /You can close this tab\. Next, run <code>carrick init<\/code> in the folder that holds your repos\./);
  assert.doesNotMatch(run.html, /<link|<script|<img|src=|href=/);
});

test("a failed workspace lookup keeps the token and still says signed in", async () => {
  const run = await roundTrip((state) => `code=accepted&state=${state}`, (url) => url.endsWith("/oauth/token") ? issued(url) : new Response("{}", { status: 503 }));
  assert.equal(run.outcome.status === "fulfilled" && run.outcome.value, "issued");
  assert.match(run.html, /<h1>Signed in to Carrick<\/h1>/);
});

test("a hung workspace lookup is bounded and the page goes out without the name", async () => {
  const started = Date.now();
  const run = await roundTrip((state) => `code=accepted&state=${state}`, (url, _events, signal) => url.endsWith("/oauth/token")
    ? issued(url)
    // Never answers; only the caller's signal ends it.
    : new Promise<Response>((_resolve, reject) => signal?.addEventListener("abort", () => reject(signal.reason), { once: true })),
  { lookupTimeoutMs: 100 });
  assert.equal(run.outcome.status === "fulfilled" && run.outcome.value, "issued");
  assert.match(run.html, /<h1>Signed in to Carrick<\/h1>/);
  assert.ok(Date.now() - started < 2000, "the lookup bound, not the login timeout, released the page");
});

test("a failed exchange shows the failure and the retry on the page", async () => {
  const run = await roundTrip((state) => `code=accepted&state=${state}`, () => new Response("secret", { status: 500 }));
  assert.equal(run.outcome.status, "rejected");
  assert.equal(run.status, 400);
  assert.match(run.html, /<h1>Sign-in failed<\/h1>/);
  assert.match(run.html, /Carrick token exchange failed \(HTTP 500\)\. Run <code>carrick login<\/code> again\./);
  assert.doesNotMatch(run.html, /secret|accepted/);
});

test("declined, invalid and missing-code callbacks get the same page with their own message", async () => {
  const declined = await roundTrip((state) => `error=access_denied&state=${state}`, issued);
  assert.equal(declined.outcome.status, "rejected");
  assert.deepEqual(declined.events, ["page"]);
  assert.match(declined.type ?? "", /^text\/html/);
  assert.match(declined.html, /<h1>Sign-in declined<\/h1>[\s\S]*Carrick authorization was declined\. Run <code>carrick login<\/code> again\./);

  let redirect = "";
  const pages: string[] = [];
  await assert.rejects(authorize({
    timeoutMs: 300, say: () => {},
    fetch: async (_url, options) => {
      redirect = JSON.parse(options?.body as string).redirect_uris[0];
      return Response.json({ client_id: "client" });
    },
    open: async (value) => {
      const state = new URL(value).searchParams.get("state");
      for (const query of ["code=x&state=wrong", `state=${state}`]) {
        const response = await fetch(`${redirect}?${query}`);
        assert.equal(response.status, 400);
        assert.match(response.headers.get("content-type") ?? "", /^text\/html/);
        pages.push(await response.text());
      }
      return true;
    },
  }), /timed out/);
  assert.match(pages[0]!, /<h1>Sign-in failed<\/h1>[\s\S]*not a valid sign-in link/);
  assert.match(pages[1]!, /<h1>Sign-in failed<\/h1>[\s\S]*no authorization code/);
});

test("the callback page escapes what it interpolates", () => {
  const html = callbackPage(`Signed in to <script>"x"</script>`, "a & 'b' `<i>`");
  assert.doesNotMatch(html, /<script>|<i>/);
  assert.match(html, /&lt;script&gt;&quot;x&quot;&lt;\/script&gt;/);
  assert.match(html, /a &amp; &#39;b&#39; <code>&lt;i&gt;<\/code>/);
});

test("login asks for the cli scope, records it, and refuses a token minted under another", async () => {
  // The registration, the authorization URL and the accepted token response
  // must all name the same scope. A server that has not deployed the `cli`
  // kind answers `mcp`, and taking that would leave a credential on disk that
  // cannot upload, with nothing to say why (seam doc §1.1, §8.1).
  assert.equal(SCOPE, "cli");
  const exchange = (scope: unknown): OAuthOptions => {
    let redirect = "";
    return {
      timeoutMs: 3000,
      say: () => {},
      fetch: async (input, options) => {
        if (String(input).endsWith("/oauth/register")) {
          const body = JSON.parse(options?.body as string);
          assert.equal(body.scope, SCOPE);
          redirect = body.redirect_uris[0];
          return Response.json({ client_id: "client" });
        }
        return Response.json({ access_token: "issued", token_type: "Bearer", scope });
      },
      open: async (value) => {
        const url = new URL(value);
        assert.equal(url.searchParams.get("scope"), SCOPE);
        await fetch(`${redirect}?code=accepted&state=${url.searchParams.get("state")}`);
        return true;
      },
    };
  };
  assert.equal(await authorize(exchange(SCOPE)), "issued");
  await assert.rejects(authorize(exchange("mcp")), /invalid OAuth token response/);
});

test("a saved credential records its scope, and one written before the field reads as mcp", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-scope-"));
  const env = { XDG_CONFIG_HOME: dir };
  try {
    saveCredential("issued", "acme", env);
    assert.equal(readCredential(env)?.scope, SCOPE);

    // Every credential on disk today predates the field. Reading one must
    // succeed and report no scope, which the Rust reader defaults to `mcp`;
    // refusing it would sign out every installed CLI on this release.
    const file = credentialPath(env);
    const value = JSON.parse(fs.readFileSync(file, "utf8"));
    delete value.scope;
    fs.writeFileSync(file, JSON.stringify(value), { mode: 0o600 });
    assert.equal(readCredential(env)?.scope, undefined);
    assert.equal(readCredential(env)?.token, "issued");

    // A scope of the wrong type is a malformed file, not an absent field.
    fs.writeFileSync(file, JSON.stringify({ ...value, scope: 7 }), { mode: 0o600 });
    assert.throws(() => readCredential(env), /carrick login/);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("login timeout closes its listener and never exchanges a code", async () => {
  let redirect = "";
  await assert.rejects(authorize({
    timeoutMs: 40, say: () => {}, open: async () => false,
    fetch: async (_url, options) => {
      redirect = JSON.parse(options?.body as string).redirect_uris[0];
      return Response.json({ client_id: "client" });
    },
  }), /timed out/);
  await assert.rejects(fetch(redirect));
});

test("browser refusal closes login without exchanging or persisting a token", async () => {
  let calls = 0;
  await assert.rejects(authorize({
    timeoutMs: 3000, say: () => {},
    fetch: async () => { calls++; return Response.json({ client_id: "client" }); },
    open: async (value) => {
      const url = new URL(value);
      const redirect = url.searchParams.get("redirect_uri")!;
      await fetch(`${redirect}?error=access_denied&state=${url.searchParams.get("state")}`);
      return true;
    },
  }), /declined/);
  assert.equal(calls, 1);
});

// carrick#1487. The cloud half is carrick-cloud `app/src/pages/oauth/revoke.ts`;
// its test sends this same request shape and pins what it revokes.
test("logout revokes the saved key with the key itself as the bearer, then signs out", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-auth-"));
  const env = { XDG_CONFIG_HOME: dir };
  const printed: string[] = [];
  const seen: { url: string; init: RequestInit | undefined }[] = [];
  try {
    saveCredential("carrick_sk_live_thismachine", "acme", env);
    const code = await logout([], {
      env, out: (text) => printed.push(text), err: (text) => printed.push(text),
      fetch: async (url, init) => { seen.push({ url: String(url), init }); return new Response(null, { status: 204 }); },
    });
    assert.equal(code, 0);
    assert.equal(seen.length, 1);
    assert.equal(seen[0]!.url, `${APP_BASE}/oauth/revoke`);
    assert.equal(seen[0]!.init?.method, "POST");
    assert.equal(seen[0]!.init?.redirect, "error");
    assert.equal(new Headers(seen[0]!.init?.headers).get("Authorization"), "Bearer carrick_sk_live_thismachine");
    assert.equal(seen[0]!.init?.body, undefined);
    assert.equal(fs.existsSync(credentialPath(env)), false);
    assert.deepEqual(printed, ["Signed out of acme.\n"]);
    assert.ok(!printed.join("").includes("app.carrick.tools/account"));
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("logout against a cloud without the revoke route, or offline, still removes the credential and says the key is live", async () => {
  const oldCloud: typeof fetch = async () => new Response("<html>Not found</html>", { status: 404, headers: { "Content-Type": "text/html" } });
  const offline: typeof fetch = async () => { throw new TypeError("fetch failed"); };
  const serverError: typeof fetch = async () => new Response("{}", { status: 500 });
  for (const request of [oldCloud, offline, serverError]) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-auth-"));
    const env = { XDG_CONFIG_HOME: dir };
    const printed: string[] = [];
    try {
      saveCredential("carrick_sk_live_thismachine", "acme", env);
      const code = await logout([], { env, fetch: request, out: (text) => printed.push(text), err: (text) => printed.push(text) });
      assert.equal(code, 0);
      assert.equal(fs.existsSync(credentialPath(env)), false);
      assert.equal(printed.length, 1);
      assert.match(printed[0]!, /^Signed out of acme on this machine, but Carrick could not revoke the key on the server\./);
    } finally { fs.rmSync(dir, { recursive: true, force: true }); }
  }
});

test("logout revokes the saved key, never CARRICK_TOKEN, and keeps the override warning", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-auth-"));
  const printed: string[] = [];
  const bearers: (string | null)[] = [];
  const request: typeof fetch = async (_url, init) => { bearers.push(new Headers(init?.headers).get("Authorization")); return new Response(null, { status: 204 }); };
  try {
    const env = { XDG_CONFIG_HOME: dir, CARRICK_TOKEN: "carrick_sk_live_cioverride" };
    saveCredential("carrick_sk_live_thismachine", "acme", { XDG_CONFIG_HOME: dir });
    assert.equal(await logout([], { env, fetch: request, out: (text) => printed.push(text) }), 0);
    assert.deepEqual(bearers, ["Bearer carrick_sk_live_thismachine"]);
    assert.deepEqual(printed, ["Signed out of acme.\n", "CARRICK_TOKEN still overrides login; unset it in your shell to sign out.\n"]);

    // Nothing saved: no request at all, and the override key is left alone.
    printed.length = 0;
    assert.equal(await logout([], { env, fetch: request, out: (text) => printed.push(text) }), 0);
    assert.equal(bearers.length, 1);
    assert.equal(printed[0], "No saved Carrick credential.\n");
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
