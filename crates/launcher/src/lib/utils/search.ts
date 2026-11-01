// The report of an answer's web search, as Rust stores it with the answer.

type SearchNote = 'none' | 'queriesOnly' | 'failed';

interface SearchSource {
  title: string;
  url: string;
  host: string;
}

export interface SearchReport {
  searched: boolean;
  queries: string[];
  sources: SearchSource[];
  suggestionHtml: string;
  note: SearchNote;
  hints: boolean;
}
