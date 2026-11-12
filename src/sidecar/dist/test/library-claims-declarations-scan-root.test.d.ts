/**
 * carrick#2474 R1 part 1, follow-up: a verified `op` verdict's `declarations`
 * are spelled from the scan root, as position rows spell `decl_file`, so the
 * two join for a service that is not at the repo root. The fixture is a repo
 * with a service under `services/web`: one package installed under the
 * service, one hoisted to the repo root. Each verdict is compared with the
 * row the `position_rows` walk writes for the same call.
 */
export {};
