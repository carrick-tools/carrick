// Barrel: every module's default export, re-exported under a distinct name.
// The import specifier is identical for all four, and each module names its
// own plugin `routes`, so nothing but the module a binding came FROM tells
// them apart.
export { default as actionsRoutes } from "./modules/actions/actions.routes.js";
export { default as sessionsRoutes } from "./modules/sessions/sessions.routes.js";
export { default as filesRoutes } from "./modules/files/files.routes.js";
export { default as logsRoutes } from "./modules/logs/logs.routes.js";
