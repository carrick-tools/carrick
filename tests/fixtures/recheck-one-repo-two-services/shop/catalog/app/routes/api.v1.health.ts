// A second route with no consumer anywhere, so the workspace holds an
// orphan as well as a matched pair.

export async function loader(): Promise<{ status: string }> {
  return { status: "ok" };
}
