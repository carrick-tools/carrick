import { reply } from "@fixture/reply";
function toDomain(d: { id: string; extra: number }) {
  return { id: d.id };
}
export async function getThing(p: string) {
  const res = await reply.get<{ id: string; extra: number }>(p);
  return toDomain(res.data);
}
