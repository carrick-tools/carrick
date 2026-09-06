// The directory form of the same flat chain: the whole route is encoded in the
// DIRECTORY name and the module is named for its role, not for a segment
// (carrick#701). It must derive exactly what the single-file spelling
// `projects.v3.$projectRef.metrics.ts` would.

type Metrics = { runs: number; failures: number };

export async function loader({
  params,
}: {
  params: { projectRef: string };
}): Promise<Metrics> {
  return { runs: Number(params.projectRef.length), failures: 0 };
}
