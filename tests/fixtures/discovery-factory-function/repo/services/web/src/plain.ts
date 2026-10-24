export function makeFormatter(prefix: string): (text: string) => string {
  function format(text: string): string {
    return prefix + text;
  }
  return format;
}

export function show(): string {
  const label = makeFormatter("> ");
  return label("hello");
}
