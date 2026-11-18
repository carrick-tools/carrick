// Not the entry file: the folder's route is index.ts alone.
export async function handleInvite(req: Request): Promise<Response> {
  const { email } = await req.json();
  return new Response(JSON.stringify({ invited: email }));
}
