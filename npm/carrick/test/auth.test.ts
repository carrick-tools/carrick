import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import http from "node:http";
import test from "node:test";
import { credentialPath, readCredential, saveCredential, removeCredential, API_BASE } from "../src/auth/credentials.ts";
import { authorize } from "../src/auth/oauth.ts";
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

test("loopback login binds state, S256, resource and redirect to the token exchange", async () => {
  let authorization: URL;
  let redirect = "";
  let exchanges = 0;
  const token = await authorize({
    timeoutMs: 3000,
    say: () => {},
    fetch: async (input, options) => {
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
      return Response.json({ access_token: "issued", token_type: "Bearer", scope: "mcp", expires_in: 31536000 });
    },
    open: async (url) => {
      authorization = new URL(url);
      assert.equal(authorization.searchParams.get("code_challenge_method"), "S256");
      assert.equal(authorization.searchParams.get("scope"), "mcp");
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
