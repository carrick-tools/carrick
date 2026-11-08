/* The GraphQL client this app sends its documents through. */
export declare function useQuery(options: { query: unknown; variables?: unknown }): unknown;
export declare function useMutation(options: { mutation: unknown }): unknown;
export declare function gql(request: { query: unknown; variables?: unknown }): Promise<unknown>;
