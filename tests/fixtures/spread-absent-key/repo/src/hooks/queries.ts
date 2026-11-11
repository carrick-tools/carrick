import { createGadget, getGadget, listGadgets, type Options } from '../sdk';

// Each hook spreads its own outer parameter (`options`) into the SDK call.
// No caller of a hook writes `client`, so the key is absent and the receiver
// holds the module client alone.
export const listGadgetsOptions = (options?: Options<{ query?: { page?: string } }>) => ({
  queryFn: async ({ signal }: { signal: AbortSignal }) => {
    const { data } = await listGadgets({ ...options, signal, throwOnError: true });
    return data;
  },
});

export const getGadgetOptions = (options: Options<{ path: { id: string } }>) => ({
  queryFn: async ({ signal }: { signal: AbortSignal }) => {
    const { data } = await getGadget({ ...options, signal, throwOnError: true });
    return data;
  },
});

export const createGadgetMutation = (options: Options<{ body: { name: string } }>) => ({
  mutationFn: async () => {
    const { data } = await createGadget({ ...options, throwOnError: true });
    return data;
  },
});
