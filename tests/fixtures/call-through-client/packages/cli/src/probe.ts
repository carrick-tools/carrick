import { HEALTH_PROBE_PATH } from "./routes/urls";

/** How the probe reads the daemon: supplied by the runtime. */
type Reader = (options: { port: number; path: string }) => Promise<unknown>;

let reader: Reader | undefined;

export function useReader(next: Reader) {
  reader = next;
}

/** The daemon's health, read over the runtime's reader. */
export async function readHealth(port: number, wait = false) {
  return reader?.({ port, path: wait ? `${HEALTH_PROBE_PATH}?wait=1` : HEALTH_PROBE_PATH });
}
