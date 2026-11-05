import { APP_BASE } from "../auth/credentials.ts";

/**
 * The dashboard page init sends a reader to: where a workspace's repos are
 * added, and where each one shows how far its index has got.
 *
 * The only page there is to send anyone to. Repos join the workspace through
 * the GitHub App and the dashboard, never from this command, because the
 * config and the index are Carrick's (carrick#3361).
 */
export function reposUrl(slug: string): string {
  return `${APP_BASE}/w/${encodeURIComponent(slug)}/repos`;
}
