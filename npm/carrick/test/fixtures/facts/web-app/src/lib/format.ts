// Money formatting for the cart and checkout pages.

export function formatPrice(cents: number): string {
  const whole = Math.floor(cents / 100);
  const rest = String(cents % 100).padStart(2, "0");
  return `$${whole}.${rest}`;
}

// Every price in a list, formatted.
export function formatAll(prices: number[]): string[] {
  return prices.map((cents) => formatPrice(cents));
}
