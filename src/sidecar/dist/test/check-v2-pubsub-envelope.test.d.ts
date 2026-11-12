/**
 * v2 check — pub/sub envelope gate (carrick#1979; real vendored pnpm + tsc).
 *
 * A subscriber's handler parameter is typed as the WHOLE message: the routing
 * key naming the topic, a variant tag, the payload and the fields the wrapper
 * stamps. A publisher is typed as the payload it passes in, or as a batch of
 * messages. Compared raw, every stamped field reads as missing. The probe
 * tests each side for a required member typed as this operation's topic
 * literal, and when exactly one side has it the pair is not compared.
 *
 * Pins the abstain AND its edges: both whole or neither whole is judged as
 * before, the gate keys on this operation's topic, it is pub/sub only, and a
 * side the decay gates decide gains no code from it.
 */
export {};
