// One third-party call, written as an absolute URL literal, with nothing
// declared about its host anywhere.
export async function sendReceipt(to: string): Promise<boolean> {
  const response = await fetch("https://api.example-mail.test/emails", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ to }),
  });
  return response.ok;
}
