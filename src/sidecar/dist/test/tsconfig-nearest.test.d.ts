/**
 * carrick#1604: a service with no config at or above its root types each file
 * under the config an editor would pick for it: the nearest `tsconfig.json`,
 * then `jsconfig.json`, in each directory from the file's up to the service
 * root.
 *
 * Before, such a service was typed as one default project under NodeNext
 * resolution, so a project that keeps its config in a subfolder (or a
 * `jsconfig.json`) read every alias and extensionless import as unresolved.
 */
export {};
