// A stand-in for a decorator-routing library: the scanner reads the shape.
export function Controller(prefix?: string): ClassDecorator {
  return () => undefined;
}

export function Post(path?: string | string[]): MethodDecorator {
  return () => undefined;
}

export function Get(path?: string | string[]): MethodDecorator {
  return () => undefined;
}
