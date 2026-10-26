import { useMutation } from "@fixture/query";
import { getStatus, listRuns, saveRun } from "@fixture/client";
import { notify } from "./notify";
import { openSession } from "./session";
declare const cache: { invalidate(key: string): void };

/** Two callbacks: the one beside the request calls only the app's own code. */
export function useSaveRun() {
  return useMutation({ mutationFn: (id: string) => saveRun({ path: { id } }), onSuccess: () => notify("saved") });
}

/** Two callbacks, one calling what nothing resolves: it could be the request. */
export function useSaveRunCached() {
  return useMutation({ mutationFn: (id: string) => saveRun({ path: { id } }), onSuccess: () => cache.invalidate("runs") });
}

/** Requests chained with the promise methods, one per element. */
export async function loadDashboard() {
  return Promise.all([
    listRuns().catch(() => null),
    getStatus().then((status) => status),
  ]);
}

/** A factory that requests something and returns an object of its own. */
export async function runSession() {
  return (await openSession()).run();
}
