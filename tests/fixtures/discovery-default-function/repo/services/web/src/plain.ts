function label(text: string): string {
  return text.trim();
}

export function describe(name: string, format: (text: string) => string = (t) => label(t)): string {
  return format(name);
}

export function run(): string {
  return describe("x");
}
