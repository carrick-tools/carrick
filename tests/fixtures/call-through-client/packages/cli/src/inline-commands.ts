import { createClient } from "./api-client";

function getClient() {
  return createClient();
}

/** The client is the value of a call, read in place (carrick#2822). */
export async function inspectRun(id: string) {
  return getClient().getRun(id);
}

export async function inspectStatus() {
  return createClient().getStatus();
}

/** A wrapper over a client factory, called from a command table (carrick#2823). */
async function showToken() {
  return createClient().getToken();
}

export const commands = [
  { path: ["token", "show"], run: () => showToken() },
];
