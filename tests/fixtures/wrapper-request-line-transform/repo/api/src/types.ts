export interface Ticket {
  id: string;
  title: string;
}

export interface TokenBody {
  scopes: string[];
}

export interface Token {
  token: string;
}

export interface NoteBody {
  text: string;
}

export interface Note {
  id: string;
  text: string;
}
