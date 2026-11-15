import { combine, OnMessage, Traced } from "@fixture/gateway";

// The package's own combinator applies the labelled decorator it is handed.
const Listens = (event: string) => combine(Traced(event), OnMessage(event));

// Calls the labelled decorator but hands back another one.
const Logs = (event: string) => {
  OnMessage(event);
  return Traced(event);
};

// An in-repo combinator, written in this file.
function compose(...decorators: MethodDecorator[]): MethodDecorator {
  return (target, key, descriptor) => {
    for (const decorator of decorators) {
      decorator(target, key, descriptor);
    }
  };
}

const Named = (event: string) => compose(OnMessage(event));

const WRAPPED = "w:const";

export class ComposedGateway {
  @Listens("w:combined")
  combined(): void {}

  @Logs("w:discarded")
  discarded(): void {}

  @Named(WRAPPED)
  named(): void {}
}
