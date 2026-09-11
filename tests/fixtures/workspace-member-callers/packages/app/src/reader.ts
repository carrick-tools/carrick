import { RunClient, createRunClient, runClientManager } from "@fixture/core/v2";
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

/**
 * The receiver is a FIELD of the enclosing class, declared by a constructor
 * parameter property, so the class body states its class (carrick#782).
 */
export class RunMetadataManager {
  constructor(private readonly apiClient: RunClient) {}

  readStreamThroughField(runId: string, streamKey: string) {
    return this.apiClient.fetchStream(runId, streamKey);
  }
}

/** A field the class body leaves unannotated states nothing. */
export class UntypedManager {
  private apiClient = new RunClient("https://runs.example.test");

  readStreamUntyped(runId: string, streamKey: string) {
    return this.apiClient.fetchStream(runId, streamKey);
  }
}

/**
 * The file states no class for the receiver — only that its value came out of
 * a binding imported from a workspace package (carrick#781). One class in that
 * package's published surface declares this member, so the edge is answerable.
 */
export function readRunByOrigin(runId: string) {
  const client = runClientManager.clientOrThrow();
  return client.subscribeToRun(runId);
}

/** The same origin, but two classes in the surface declare this member. */
export function readStreamByOrigin(runId: string, streamKey: string) {
  const client = runClientManager.clientOrThrow();
  return client.fetchStream(runId, streamKey);
}

/**
 * A nested parameter shadows the origin, and the nested call site is folded
 * into this function's — so the origin must not answer for it.
 */
export function readContestedByOrigin(runId: string) {
  const client = runClientManager.clientOrThrow();
  const nested = (client: VendorClient) => client.subscribeToRun(runId);
  return nested;
}

/**
 * A call at MODULE SCOPE (carrick#965). It sits inside no function, so until
 * the file itself became an owner nothing recorded it, and `createRunClient`
 * read as having no callers at all.
 */
export const bootClient = createRunClient("https://runs.example.test");
