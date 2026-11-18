export async function both(b: string) {
  await fetch(`${b}/first`);
  return (await fetch(`${b}/second`)).json();
}
