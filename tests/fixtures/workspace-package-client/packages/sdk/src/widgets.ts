import { clientManager } from "@fixture/core/v2";
import { otherManager } from "@fixture/other";

// The shape carrick#666 is about: the client is never imported. It is asked
// for from a factory that is, parked on a local, and called. The site states
// no path and no verb, and neither does this file.
export async function fetchWidget(id: string) {
  const coreClient = clientManager.clientOrThrow();
  return coreClient.retrieveWidget(id);
}

// The same member name reached through the OTHER package's factory. Only the
// receiver's origin separates the two, so a join that ignored it would give
// both sites the same route.
export async function fetchWidgetElsewhere(id: string) {
  const otherClient = otherManager.clientOrThrow();
  return otherClient.retrieveWidget(id);
}

// A receiver with no import behind it. `retrieveWidget` is an unambiguous name
// across both surfaces and still states nothing about which one this is.
export async function fetchWidgetUnbound(id: string) {
  const localClient = makeLocalClient();
  return localClient.retrieveWidget(id);
}

// A name two modules of the core surface declare differently. A site could be
// reaching either, so it reaches neither.
export async function listWidgets() {
  const coreClient = clientManager.clientOrThrow();
  return coreClient.list();
}

// A chained call on the result of a client call. The receiver of `.catch` is a
// promise, not the client, and the chain is one outbound request.
export async function archiveWidget(id: string) {
  const coreClient = clientManager.clientOrThrow();
  return coreClient.archiveWidget(id).catch(() => undefined);
}

function makeLocalClient() {
  return {
    retrieveWidget(id: string) {
      return Promise.resolve({ id });
    },
  };
}
