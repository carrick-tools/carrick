import { reply } from "@fixture/reply";
function fromDto(d: { n: number }) {
  return d.n;
}
export async function nums(p: string) {
  return (await reply.get<{ n: number }[]>(p)).data.map(fromDto);
}
