/**
 * A request body whose field is fed from a UI state hook holding a literal
 * union. Reported as a suspected false positive: the field was judged as
 * `string`, which a producer declaring the union rejects.
 *
 * The hook is declared here rather than imported so the fixture needs no
 * framework installed, and with the same shape the real one has — the state
 * type flows out through a tuple whose second member is a setter over
 * `S | ((prev: S) => S)`, which is where a literal type could widen.
 */

type SetStateAction<S> = S | ((previous: S) => S);
type Dispatch<A> = (value: A) => void;

declare function useState<S>(initial: S | (() => S)): [S, Dispatch<SetStateAction<S>>];

declare function post(url: string, init: { method: string; body: string }): Promise<void>;

/** The reported shape: the state type is stated as a union at the hook call. */
export async function submitStatedUnion(): Promise<void> {
  const [mode, setMode] = useState<'draft' | 'published'>('draft');
  setMode('published');
  await post('/api/documents', {
    method: 'POST',
    body: JSON.stringify({ mode, title: 'untitled' }),
  });
}

/** The same shape with the state type left to inference, where `string` is the
 * compiler's own answer: an unconstrained type parameter widens the literal. */
export async function submitInferredLiteral(): Promise<void> {
  const [mode] = useState('draft');
  await post('/api/documents', {
    method: 'POST',
    body: JSON.stringify({ mode }),
  });
}
