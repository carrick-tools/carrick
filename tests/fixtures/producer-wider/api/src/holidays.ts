export interface HolidayRow {
  id: string;
  parentId: string | null;
}

const rows: HolidayRow[] = [];

export async function listHolidays() {
  return rows.map((row) => ({
    id: row.id,
    scope: row.parentId ? "specific" : "all",
  }));
}
