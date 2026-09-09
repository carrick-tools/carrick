// One off switch per surface, in one place.
//
// Every surface this server offers can be turned off on its own, defaulting on,
// and turning one off leaves the others exactly as they were (design record
// 2026-09-09, "Off switches, one per thing"). `CARRICK_CHANNEL=off` stays the
// blunt instrument that silences delivery entirely.
//
// A client states them twice over: once in `initializationOptions` at startup,
// and again in `workspace/didChangeConfiguration` when a person changes a
// setting mid-session, which is why one parser reads both shapes. VS Code sends
// `{ settings: { carrick: { ... } } }` for the notification; a generic client
// that only has `initializationOptions` sends the flat object. Either is
// accepted, and anything that is not a boolean leaves the default alone rather
// than reading as false.
//
// `boundarySurface` is not an off switch and not a preference: it is the client
// saying it has somewhere else to put the boundary, which the VS Code extension
// does because it owns a status bar item. It decides placement, never whether
// the boundary is stated at all. See diagnostics.ts for what it changes.

export type Surfaces = {
  /** Publish verdict diagnostics at all. */
  diagnostics: boolean;
  /** State the boundary. Where it lands depends on `boundarySurface`. */
  boundary: boolean;
  /** Render code lenses on the rows the index knows something about. */
  codeLens: boolean;
  /**
   * The client shows the boundary somewhere that is not the Problems list, so
   * the per-file Information diagnostic is not published to it. False for a
   * client that says nothing, which keeps the fallback.
   */
  boundarySurface: boolean;
};

export const DEFAULT_SURFACES: Surfaces = {
  diagnostics: true,
  boundary: true,
  codeLens: true,
  boundarySurface: false,
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * The `carrick` block of whatever a client sent, flat or nested.
 *
 * `initializationOptions` is already the block. A
 * `workspace/didChangeConfiguration` payload wraps it in `settings.carrick`,
 * and some clients send `settings` alone with the section stripped.
 */
function block(params: unknown): Record<string, unknown> {
  if (!isRecord(params)) return {};
  const settings = params["settings"];
  if (isRecord(settings)) {
    const scoped = settings["carrick"];
    return isRecord(scoped) ? scoped : settings;
  }
  const scoped = params["carrick"];
  if (isRecord(scoped)) return scoped;
  return params;
}

/**
 * Read the switches a client states, over the ones already in force.
 *
 * A key the client omits keeps its current value: a notification that carries
 * one setting must not silently reset the rest to their defaults.
 */
export function readSurfaces(params: unknown, current: Surfaces = DEFAULT_SURFACES): Surfaces {
  const stated = block(params);
  const read = (key: keyof Surfaces): boolean => {
    const value = stated[key];
    return typeof value === "boolean" ? value : current[key];
  };
  return {
    diagnostics: read("diagnostics"),
    boundary: read("boundary"),
    codeLens: read("codeLens"),
    boundarySurface: read("boundarySurface"),
  };
}
