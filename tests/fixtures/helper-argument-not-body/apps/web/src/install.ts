import { postEvent } from "@fixture/client";
import { configureTracking } from "./track";
import type { EventBody } from "./types";

export async function sendEvent(event: EventBody): Promise<void> {
  await postEvent({ body: event });
}

export function installTracking(): void {
  configureTracking({ send: sendEvent });
}
