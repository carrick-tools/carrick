export async function wrapped(b: string) {
  const body = await (await fetch(`${b}/env`)).json();
  return { status: 200, body };
}
