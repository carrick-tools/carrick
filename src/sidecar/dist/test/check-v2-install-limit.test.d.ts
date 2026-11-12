/**
 * v2 check core: the workspace install has its own time limit (carrick#3021).
 *
 * A package manager that cannot reach the registry retries its fetches for
 * minutes, or for ever behind a dropped network. The check must come back
 * inside its limit, say why, and mark every pair as not checked.
 */
export {};
