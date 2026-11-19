export async function pingOther(): Promise<Response> {
  return fetch(`${import.meta.env.VITE_OTHER_URL}/x`, { method: "POST" });
}
