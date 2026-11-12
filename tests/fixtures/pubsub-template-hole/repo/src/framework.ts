// Stand-ins for a framework that calls resolver methods itself and fills
// their parameters from the request: nothing in the service calls them.
export function Injectable(): ClassDecorator {
  return () => undefined;
}
export function Resolver(): ClassDecorator {
  return () => undefined;
}
export function Mutation(): MethodDecorator {
  return () => undefined;
}
export function Subscription(): MethodDecorator {
  return () => undefined;
}
export function Arg(_name: string): ParameterDecorator {
  return () => undefined;
}
export function CurrentUser(): ParameterDecorator {
  return () => undefined;
}
export interface User {
  id: string;
  email: string;
}
