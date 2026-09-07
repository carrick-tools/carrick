import { RunClient } from "@fixture/core/v2";
import type { VendorClient } from "vendor-runs";

/**
 * The receiver is a parameter the file DECLARES the class of. Nothing here
 * imports `subscribeToRun`, and the class is published by a sibling package
 * under a manifest subpath.
 */
export function readRun(runId: string, client: RunClient) {
  return client.subscribeToRun(runId);
}

/** The receiver is constructed here, so the file states the class outright. */
export function readStream(runId: string, streamKey: string) {
  const client = new RunClient("https://runs.example.test");
  return client.fetchStream(runId, streamKey);
}

/** A receiver the file says nothing about resolves to nothing. */
export function readUnbound(runId: string, client: unknown) {
  return (client as { subscribeToRun: (id: string) => unknown }).subscribeToRun(
    runId,
  );
}

/** A receiver whose class an EXTERNAL package declares has no source here. */
export function readVendor(runId: string, client: VendorClient) {
  return client.subscribeToRun(runId);
}

/** Two bindings of one name in one scope state nothing, so neither resolves. */
export function readAmbiguous(runId: string, client: RunClient) {
  const inner = (client: VendorClient) => client.subscribeToRun(runId);
  return inner;
}
