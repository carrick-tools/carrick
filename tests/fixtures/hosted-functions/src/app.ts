// The web app's own code. It calls no function yet: the consumer half of this
// fixture (the client calls that name a function by a string) comes later.
export function greeting(name: string): string {
  return `hello ${name}`;
}
