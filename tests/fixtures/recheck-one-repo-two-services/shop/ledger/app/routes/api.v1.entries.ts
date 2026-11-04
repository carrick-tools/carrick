// GET /api/v1/entries — derived from the file name and the exported handler.
// Nothing reads it and it reads nothing from the other services, so an edit
// here is the edit of a service no other service is paired with.

export async function loader(): Promise<{ count: number }> {
  return { count: 0 };
}
