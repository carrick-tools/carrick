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
