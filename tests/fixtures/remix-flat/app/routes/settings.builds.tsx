// The precision case (carrick#703): a route module whose write handler
// dispatches on a form discriminator. The discriminator is a schema literal
// and a form field id — it appears in the module's bytes exactly the way a
// route path would — but nothing registers it as a route. The convention
// states this module's whole route set from its exports, so a row the model
// invents from that literal has no witness and is discarded.

import { z } from "zod";

const BuildSettingsForm = z.object({
  action: z.literal("update-build-settings"),
  autoDeploy: z.boolean(),
});

type BuildSettings = { autoDeploy: boolean };

export async function loader(): Promise<BuildSettings> {
  // An outbound call, so the module raises a candidate the analyzer answers at.
  const response = await fetch("/internal/audit");
  return (await response.json()) as BuildSettings;
}

export async function action({
  request,
}: {
  request: Request;
}): Promise<BuildSettings> {
  const form = BuildSettingsForm.parse(await request.json());
  return { autoDeploy: form.autoDeploy };
}

export default function BuildSettingsPage() {
  return null;
}
