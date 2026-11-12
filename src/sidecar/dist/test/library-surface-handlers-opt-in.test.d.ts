/**
 * carrick#2966: `list_library_surface` lists handler-parameter members
 * (carrick#2674 S3) only when the request sets `handlers: true`. Without it the
 * handler pass does not run, and the hash is the same either way.
 *
 * The package below is invented and its declarations are hand-written.
 */
export {};
