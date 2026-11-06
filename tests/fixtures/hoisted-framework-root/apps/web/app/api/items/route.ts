// Next.js App Router: app/api/items/route.ts -> /api/items

export async function GET(): Promise<Response> {
  return Response.json({ items: [] });
}
