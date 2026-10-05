import { useCallback } from "a-ui-runtime";

export function useBoardEditor({ saveUrl }: { saveUrl: string }) {
  const save = useCallback(
    async (data: FormData) => {
      const response = await fetch(saveUrl, { method: "POST", body: data });
      return response.ok;
    },
    [saveUrl],
  );
  return { save };
}
