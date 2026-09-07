// An observability decorator named after an HTTP method the spec defines but
// nobody routes on. It is here to be ignored.
export function Trace(): MethodDecorator {
  return () => undefined;
}
