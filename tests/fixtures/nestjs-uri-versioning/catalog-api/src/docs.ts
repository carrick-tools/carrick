// A documentation tag that takes one object, from another module.
export function Docs(options: { path: string; version: string }): ClassDecorator {
  return () => undefined;
}
