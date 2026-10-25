import { getRun, ping, readRun, readRunStamped, send, transport } from "@fixture/client";
import { createClient } from "./api-client";
import { createModelProbedClient, createProbedClient } from "./probed-client";
import type { Run, SavedRun, StampedRun, Status } from "./types";

/** A command that calls one client method. */
export async function showRun(id: string) {
  const client = createClient();
  const run: Run = await client.getRun(id);
  return run;
}

/** A command that calls another command. */
export async function showLatest() {
  const run = await showRun("latest");
  return run;
}

export async function listAll() {
  const client = createClient();
  const runs = await client.listRuns();
  return runs;
}

/** The model stated this one correctly, with the type the call expects. */
export async function showStatus() {
  const client = createClient();
  const status: Status = await client.getStatus();
  return status;
}

async function loadToken() {
  const client = createClient();
  const token = await client.getToken();
  return token;
}

async function save(id: string, token: unknown) {
  const client = createClient();
  const saved = await client.saveRun(id, String(token));
  return saved;
}

/** Both functions reach a request. The row is the call to `save`. */
export async function saveWithToken(id: string) {
  const saved: SavedRun = await save(id, await loadToken());
  return saved;
}

/** A helper that calls two client methods reaches two operations. */
async function loadRunAndList(id: string) {
  const client = createClient();
  return [await client.getRun(id), await client.listRuns()];
}

export async function showBoth(id: string) {
  const both = await loadRunAndList(id);
  return both;
}

/** Every method of this client also requests the health route. */
export async function showProbed(id: string) {
  const probed = createProbedClient();
  const run = await probed.getRun(id);
  return run;
}

/** Nothing says what `deps.client` is. */
export async function showInjected(id: string, deps: { client: any }) {
  const run = await deps.client.getRun(id);
  return run;
}

/** The function called writes its URL from its parameters alone. */
export async function showVia(base: string, path: string) {
  const reply = await send(base, path);
  return reply;
}

/** The site writes the route itself. */
export async function showOrders(base: string) {
  const orders = await send(base, "/orders");
  return orders;
}

/** The site also writes a request of its own. */
export async function showAndNotify(id: string) {
  const run = await showRun(id).finally(() => fetch("/api/v1/audit"));
  return run;
}

/** Only the model states the request this function makes. */
export async function showPing() {
  const reply = await ping();
  return reply;
}

/** The request this reaches is one the API serves under another verb. */
export async function archive(id: string) {
  const client = createClient();
  const archived = await client.archiveRun(id);
  return archived;
}

/** Requests the path it is handed, after asking for a token. */
async function authorized(base: string, path: string) {
  const token = await loadToken();
  return fetch(`${base}${path}`, { headers: { authorization: String(token) } });
}

/** The site writes the route. The function it calls reaches one request. */
export async function showWidgets(base: string) {
  const widgets = await authorized(base, "/api/v1/widgets");
  return widgets;
}

/** Reports that the work finished. */
async function audited<T>(work: Promise<T>) {
  const client = createClient();
  const result = await work;
  await client.recordAudit({ finished: true });
  return result;
}

/** The site makes a request of its own, to a URL only it knows. */
export async function postAudited(target: string) {
  const reply = await audited(fetch(target, { method: "POST" }));
  return reply;
}

/** Asks for a token, then requests a URL only the model reads. */
async function tokenThenFetch(target: string) {
  const token = await loadToken();
  return fetch(target, { headers: { authorization: String(token) } });
}

/** The function called reaches the token request and one no fact states. */
export async function showTokened(target: string) {
  const reply = await tokenThenFetch(target);
  return reply;
}

/** The function called hands back its request's parsed body. */
export async function showRead(id: string) {
  const run: Run = await readRun(id);
  return run;
}

/** The function called hands back the body inside an envelope of its own. */
export async function showStamped(id: string) {
  const stamped: StampedRun = await readRunStamped(id);
  return stamped;
}

/** The caller's copy of the type requires a field the API no longer sends. */
export async function showReadStale(id: string) {
  const run: SavedRun = await readRun(id);
  return run;
}

/** Every method of this client also makes a request only the model reads, and drops it. */
export async function showModelProbed(id: string, origin: string) {
  const probed = createModelProbedClient(origin);
  const run = await probed.getRun(id);
  return run;
}

/** Asks for a token and hands it to the request it returns. */
async function authorizedRun(id: string) {
  const token = await loadToken();
  return getRun({ client: transport, path: { id }, body: token });
}

/** The function called reaches the token request and the run request. */
export async function showAuthorized(id: string) {
  const run = await authorizedRun(id);
  return run;
}

/** Reads one run; only this file calls it, and the call below holds the row. */
async function runOf(id: string) {
  return getRun({ client: transport, path: { id } });
}

export async function showRunOf(id: string) {
  const run = await runOf(id);
  return run;
}
