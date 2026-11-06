// A stand-in for whatever decorator-routing library a service uses. The
// scanner reads the SHAPE of the decorators and the names the library gives
// its versioning, never this module's name.
export interface ControllerOptions {
  path?: string | string[];
  version?: string | symbol | string[];
  scope?: number;
}

export const VERSION_NEUTRAL = Symbol("neutral");

export enum VersioningType {
  URI = 0,
  HEADER = 1,
}

export interface VersioningOptions {
  type: VersioningType;
  defaultVersion?: string | symbol | string[];
  prefix?: string | false;
}

export interface App {
  enableVersioning(options: VersioningOptions): void;
}

export function createApp(): App {
  return { enableVersioning: () => undefined };
}

export function Controller(prefix?: string | ControllerOptions): ClassDecorator {
  return () => undefined;
}

export function Version(version: string | symbol | string[]): MethodDecorator {
  return () => undefined;
}

export function Get(path?: string): MethodDecorator {
  return () => undefined;
}

export function Post(path?: string): MethodDecorator {
  return () => undefined;
}
