// A documentation tag that also takes one string argument, from a different
// module than the verbs: the tie-break the prefix rule has to survive.
export function ApiTags(tag: string): ClassDecorator {
  return () => undefined;
}
