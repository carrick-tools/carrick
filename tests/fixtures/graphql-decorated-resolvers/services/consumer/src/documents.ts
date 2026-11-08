const gql = (strings: TemplateStringsArray): string => strings.join("");

const ME = gql`
  query Me {
    me {
      id
    }
  }
`;

const RENAME = gql`
  mutation Rename {
    renameThing {
      id
    }
  }
`;

const BY_STRING = gql`
  query ByString {
    byString {
      id
    }
  }
`;

const EVENTS = gql`
  subscription Events {
    events {
      id
    }
  }
`;

const EVERYTHING = gql`
  query Everything {
    everything {
      id
    }
  }
`;

const OPTIONED = gql`
  query Optioned {
    viaOptions {
      id
    }
  }
`;

export async function send(query: string): Promise<unknown> {
  const res = await fetch(`${process.env.BACKEND_URL}/graphql`, {
    method: "POST",
    body: JSON.stringify({ query }),
  });
  return res.json();
}

export const documents: string[] = [ME, RENAME, BY_STRING, EVENTS, EVERYTHING, OPTIONED];
