/** Pick the repo a service name names. Pure: it reads the list it is given. */
export function matchService(repos: unknown[], name: string): unknown {
  return repos.find((repo) => (repo as { name: string }).name === name) ?? null;
}
