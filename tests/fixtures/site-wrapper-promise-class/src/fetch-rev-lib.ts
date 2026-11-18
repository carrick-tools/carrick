export async function audited(b: string) {
  const r = await (await fetch(`${b}/a`)).json();
  await fetch(`${b}/audit`);
  return r;
}
