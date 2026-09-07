// A stand-in for whatever decorator-routing library a service uses. The
// scanner reads the SHAPE of the decorators, never this module's name.
export function Controller(prefix: string): ClassDecorator {
  return () => undefined;
}

export function Get(path?: string): MethodDecorator {
  return () => undefined;
}

export function Post(path?: string): MethodDecorator {
  return () => undefined;
}

// The server a service registers its routes on, so a registration written with
// an env-backed PREFIX has somewhere to be written. A stand-in like the
// decorators above: the scanner reads the shape, never this module's name.
export const app = {
  get(path: string, handler: () => unknown): void {
    void path;
    void handler;
  },
};
