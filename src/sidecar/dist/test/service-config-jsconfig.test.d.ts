/**
 * carrick#3497: a service whose only root config is `jsconfig.json` is typed
 * under it, not under a `tsconfig.json` above the service.
 *
 * tsserver types a JavaScript file under the jsconfig in its own directory,
 * so a service-root `jsconfig.json` outranks a `tsconfig.json` above the service
 * (after the service's own tsconfig names). With no config above it, the answer
 * stays undefined and the per-file search reads the same jsconfig.
 * A service that keeps any tsconfig name at its root is unchanged.
 */
export {};
