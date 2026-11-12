// Fields a constructor sets from its parameters (carrick#3678 S6-2). Each
// probe is a call of a `p_field_*` function whose argument is `this.<field>`
// read in a method of the class; the classes are constructed below them, so
// the test finds a probe by its callee name. Every parameter is typed
// `string`: a literal type would be read from the type first.
export {};

declare function p_field_lit(name: string): void;
declare function p_field_param(name: string): void;
declare function p_field_hub(name: string): void;
declare function p_field_assign(name: string): void;
declare function p_field_twice(name: string): void;
declare function p_field_spread(name: string): void;
declare function p_field_typed(name: string): void;
declare function p_field_fallback(name: string): void;

// A module-level construction with a literal.
class LitHub {
  constructor(private channel: string) {}

  listen(): void {
    p_field_lit(this.channel);
  }
}

const litHub = new LitHub("orders.lit");
litHub.listen();

// A construction filled by a function's parameter, with one caller.
class ParamHub {
  constructor(private pattern: string) {}

  listen(): void {
    p_field_param(this.pattern);
  }
}

function subscribe(channel: string): void {
  new ParamHub(channel).listen();
}

subscribe("orders.a3.*");

// A construction filled by a method's parameter, the method called on a
// module instance.
class InnerSub {
  constructor(private pattern: string) {}

  listen(): void {
    p_field_hub(this.pattern);
  }
}

class MethodHub {
  subscribe(channel: string): void {
    new InnerSub(channel).listen();
  }
}

const methodHub = new MethodHub();
methodHub.subscribe("orders.hub");

// `this.topic = topic` in the constructor body.
class AssignHub {
  private topic: string;

  constructor(topic: string) {
    this.topic = topic;
  }

  send(): void {
    p_field_assign(this.topic);
  }
}

new AssignHub("orders.assign").send();

// A class constructed twice with different literals.
class TwiceHub {
  constructor(private topic: string) {}

  send(): void {
    p_field_twice(this.topic);
  }
}

new TwiceHub("orders.one").send();
new TwiceHub("orders.two").send();

// A class constructed with a spread: refused, so it is not read through
// its constructions.
class SpreadHub {
  constructor(private topic: string) {}

  send(): void {
    p_field_spread(this.topic);
  }
}

const spreadArgs: [string] = ["orders.spread"];
new SpreadHub(...spreadArgs).send();

// A field the compiler types as one literal: read from its type.
class TypedHub {
  constructor(private topic: "orders.typed") {}

  send(): void {
    p_field_typed(this.topic);
  }
}

new TypedHub("orders.typed").send();

// A field with a fallback the constructor writes: not read through its
// constructions by the text reader.
class FallbackHub {
  private topic: string;

  constructor(topic?: string) {
    this.topic = topic ?? "orders.fallback";
  }

  send(): void {
    p_field_fallback(this.topic);
  }
}

new FallbackHub("orders.given").send();
