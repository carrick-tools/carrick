const BASE = "http://api.example.com";

export async function literalBase(other: boolean) {
  if (other) {
    const BASE = "http://other.example.com";
    return fetch(`${BASE}/status`, { method: "DELETE" });
  }
  return null;
}
