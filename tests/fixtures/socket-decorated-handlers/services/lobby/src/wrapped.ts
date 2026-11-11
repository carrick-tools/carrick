import { OnMessage, Traced } from "@fixture/gateway";

// An in-repo function that composes decorators, as a decorator factory does.
function compose(...decorators: MethodDecorator[]): MethodDecorator {
  return (target, key, descriptor) => {
    for (const decorator of decorators) {
      decorator(target, key, descriptor);
    }
  };
}

// Wraps the labelled decorator: the factory's body is the library site.
const Handles = (event: string) => compose(OnMessage(event));

// Wraps an unlabelled decorator: nothing in its body is a library site.
const Tagged = (label: string) => compose(Traced(label));

export class WrappedGateway {
  @Handles("w:wrapped")
  wrapped(): void {}

  @Tagged("w:tagged")
  tagged(): void {}
}
