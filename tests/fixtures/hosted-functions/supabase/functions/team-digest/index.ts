Deno.serve(async (req) => {
  const { teamId } = await req.json();
  return new Response(JSON.stringify({ sent: true, teamId }));
});
