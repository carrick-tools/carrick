import { clientManager } from "@fixture/core/v2";
import { otherManager } from "@fixture/other";

// One local name bound from two different packages in one file. Which surface
// the receiver belongs to is what the join runs on, and here the file does not
// say, so neither site resolves.
export async function fetchFromCore(id: string) {
  const widgetClient = clientManager.clientOrThrow();
  return widgetClient.retrieveWidget(id);
}

export async function fetchFromOther(id: string) {
  const widgetClient = otherManager.clientOrThrow();
  return widgetClient.retrieveWidget(id);
}
