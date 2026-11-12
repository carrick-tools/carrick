/**
 * carrick#2817: the `result` claim, judged over every overload on a receiver.
 * It is verified when the claimed result (the whole return, one member of it,
 * or one parse method) is a candidate of at least one signature, and refused
 * with `result_not_candidate` otherwise. A make or op claim may carry a
 * `result`; the claim it sits on is judged as if it did not.
 */
export {};
