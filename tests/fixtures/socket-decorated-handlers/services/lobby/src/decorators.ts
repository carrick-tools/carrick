import { OnMessage } from "@fixture/gateway";

function compose(...decorators: MethodDecorator[]): MethodDecorator {
  return (target, key, descriptor) => {
    for (const decorator of decorators) {
      decorator(target, key, descriptor);
    }
  };
}

// A decorator factory another file applies.
export const Imported = (event: string) => compose(OnMessage(event));
