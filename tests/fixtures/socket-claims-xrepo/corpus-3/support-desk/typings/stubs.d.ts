// Ambient stubs — keep types resolvable without npm install.
// Just enough surface for the call sites; the scanner reads AST structure.

declare module "@nestjs/common" {
  export function Controller(prefix?: string): ClassDecorator;
  export function Get(path?: string): MethodDecorator;
  export function Param(name?: string): ParameterDecorator;
}

