import { useBoardEditor } from "./useBoardEditor";
import { useBoardSync } from "./useBoardSync";

export function BoardPage({ boardId }: { boardId: string }) {
  const saveUrl = `/resources/boards/${boardId}/widgets`;
  const { save } = useBoardEditor({ saveUrl });
  const { sync, peek } = useBoardSync(`/resources/boards/${boardId}/sync`, { method: "PUT" });
  return (
    <div>
      <button onClick={() => save(new FormData())}>Save</button>
      <button onClick={() => sync()}>Sync</button>
      <button onClick={() => peek()}>Peek</button>
    </div>
  );
}
