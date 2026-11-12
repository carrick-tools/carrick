/**
 * A callee with several type parameters is not retyped by writing one type
 * argument: TypeScript does not infer the rest once any is written, so each
 * takes its default instead. A hook whose variables parameter defaults to
 * `void` then rejects the options the source passed it, and that diagnostic,
 * located at the call's own argument, is about the written argument and not
 * about anything the consumer reads.
 *
 * The libraries below are hand-written and synthetic. `useJob` declares
 * `TVariables = void` and `TError = Error` beside `TData`, all three
 * mentioned by the options parameter, so each is inferred from the options
 * object in source. `fetchJob` is the control: one type parameter. `send`
 * is the other control: extra parameters whose defaults are `any`, so a
 * written first argument changes nothing the call can fail on.
 */
export {};
