// The control: a service calling its own surface over loopback. Its origin is
// this machine and classifies nothing, so it is the one absolute origin the
// key still strips.
export async function readOwnHealth(): Promise<boolean> {
  const response = await fetch("http://localhost:7100/emails", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({}),
  });
  return response.ok;
}
