import { send } from "@fixture/client";
import { JobClient } from "./job-client";

/** A client method whose request sits in another file, and is not read. */
export async function closeJob(client: JobClient, path: string) {
  const outcome = await client.close(path);
  return outcome;
}

/** The calling code builds the URL the call is handed. */
export async function showAccounts(base: string) {
  const url = `${base}/api/v1/accounts`;
  const accounts = await send(url, "");
  return accounts;
}

/** The same, through a `URL` object read back as text. */
export async function showAccount(base: string, id: string) {
  const url = new URL(`${base}/api/v1/accounts/${id}`);
  const account = await send(url.toString(), "");
  return account;
}

import { JobQueue } from "./job-queue";
import { readHealth } from "./probe";
import { report } from "./report";

/** Calls into a queue client whose requests go through functions it is handed. */
export async function work(jobs: JobQueue, id: string) {
  const claimed = await jobs.claim(5);
  const released = await jobs.release(id);
  const pong = await jobs.ping();
  return { claimed, released, pong };
}

/** A call whose request is made by the call written in its argument. */
export async function showHealth(port: number) {
  return report(await readHealth(port));
}

/** The same call, which the model keyed by the outer function's name. */
export async function showReport(port: number) {
  return report(await readHealth(port));
}
