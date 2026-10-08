// A CommonJS module written in TypeScript.
function roundCents(amount: number): number {
  return Math.round(amount);
}

module.exports = { roundCents };
