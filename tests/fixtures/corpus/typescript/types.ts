/**
 * Shared TypeScript types for syntext client.
 */

export interface SearchResult {
  query: string;
  matches: Match[];
  elapsedMs: number;
}

export interface Match {
  path: string;
  lineNumber: number;
  lineContent: string;
  byteOffset: number;
}

export interface QueryOptions {
  batchSize?: number;
  caseSensitive?: boolean;
  pathFilter?: string;
  maxResults?: number;
}
