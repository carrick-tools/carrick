import router, { Reply, Request } from "fixture-router";

declare const process: { env: { [key: string]: string | undefined } };

function ok(_request: Request, reply: Reply): void {
  reply.send("ok");
}

// Run with no settings, each function serves its routes at the default path.

export function widgetRoutes(settings: { widgetsAt?: string }): void {
  const app = router();
  // A local whose default is written with `||`.
  const widgetsAt = settings.widgetsAt || "/widgets";
  app.get(widgetsAt, ok);
  app.get(`${widgetsAt}/:widgetId`, ok);
}

export function gadgetRoutes(settings: { gadgetsAt?: string }): void {
  const app = router();
  // A local whose default is written with `??`.
  const gadgetsAt = settings.gadgetsAt ?? "/gadgets";
  app.get(gadgetsAt, ok);
  app.get(gadgetsAt + "/:gadgetId", ok);
}

export function partRoutes(settings: { partsAt?: string }): void {
  const app = router();
  // A destructuring default.
  const { partsAt = "/parts" } = settings;
  app.get(partsAt, ok);
  app.get(`${partsAt}/:partId`, ok);
}

export function toolRoutes(toolsAt = "/tools"): void {
  const app = router();
  // A parameter default.
  app.get(toolsAt, ok);
  app.get(`${toolsAt}/:toolId`, ok);
}

export function boltRoutes({ boltsAt = "/bolts" }: { boltsAt?: string }): void {
  const app = router();
  // A default on a key of a destructured parameter.
  app.get(boltsAt, ok);
  app.get(`${boltsAt}/:boltId`, ok);
}

export function nutRoutes(): void {
  const app = router();
  // A destructuring default over a value read at runtime.
  const { nutsAt = "/nuts" }: { nutsAt?: string } = JSON.parse("{}");
  app.get(nutsAt, ok);
}

export function gearRoutes(settings: { gearsAt?: string }): void {
  const app = router();
  // A default that is itself read at runtime: no path is written whole.
  const gearsAt = settings.gearsAt ?? process.env.GEARS_AT;
  app.get(`${gearsAt}`, ok);
  // A local assigned again: what it holds at the call is not its default.
  let cogsAt = settings.gearsAt ?? "/cogs";
  cogsAt = String(process.env.COGS_AT);
  app.get(cogsAt, ok);
}
