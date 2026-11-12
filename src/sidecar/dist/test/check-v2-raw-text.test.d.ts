/**
 * carrick#1842: a consumer that reads the body as raw text states no
 * structural contract, so the pair is unverifiable, as a body of bytes is
 * (carrick#1812).
 *
 * The capture records the inferrer's mark on the alias (`raw_text_read`), the
 * stub carries it to every scan that checks against this service, and the
 * judge gates on it beside the bytes gate, so it holds whether the sides
 * agree or not. A `string` consumer with no mark is still compared, and a
 * side that is `any` keeps its own reason.
 */
export {};
