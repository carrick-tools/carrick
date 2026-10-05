import { useBoardEditor } from "./useBoardEditor";

export function BoardPage({ boardId }: { boardId: string }) {
  const saveUrl = `/resources/boards/${boardId}/widgets`;
  const { save } = useBoardEditor({ saveUrl });
  return <button onClick={() => save(new FormData())}>Save</button>;
}
