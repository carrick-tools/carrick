// A stand-in for whatever decorator-routing library a service uses. The
// scanner reads the SHAPE of the decorators, never this module's name.
export interface ControllerOptions {
  path?: string | string[];
  version?: string;
  scope?: number;
}

export function Controller(prefix?: string | ControllerOptions): ClassDecorator {
  return () => undefined;
}

export function Get(path?: string): MethodDecorator {
  return () => undefined;
}

export function Post(path?: string): MethodDecorator {
  return () => undefined;
}
