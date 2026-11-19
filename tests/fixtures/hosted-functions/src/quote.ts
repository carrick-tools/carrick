export async function requestQuote(): Promise<Response> {
  return fetch(`${import.meta.env.VITE_SUPABASE_URL}/functions/v1/price-quote`, {
    method: "POST",
    body: JSON.stringify({ items: [] }),
  });
}
