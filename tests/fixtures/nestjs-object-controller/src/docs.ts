// A documentation decorator that takes one object argument, from a different
// module than the verbs: it states no prefix.
export function Docs(options: { path: string }): ClassDecorator {
  return () => undefined;
}
