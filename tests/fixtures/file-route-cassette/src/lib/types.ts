export interface Note {
  id: string;
  title: string;
  body: string;
}

export interface NoteList {
  notes: Note[];
  total: number;
}
