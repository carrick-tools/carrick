// The web app's own code that calls no function. The client calls that name
// a function by a string are in direct.ts, digest.ts, page.ts and method.ts.
export function greeting(name: string): string {
  return `hello ${name}`;
}
