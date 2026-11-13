// The table members an action is handed: a factory returns them, and each
// sends a command through one module client.
import { TableClient, GetRow, QueryRows } from 'fixture-table';

const table = TableClient.from({});

function pages(run: (page: { index: string }) => Promise<unknown>): Promise<unknown> {
  return run({ index: 'by-owner' });
}

export function tableDeps() {
  return {
    loadRow: (key: string) => table.send(new GetRow({ key })),
    listRows: () => pages((page) => table.send(new QueryRows(page))),
  };
}
