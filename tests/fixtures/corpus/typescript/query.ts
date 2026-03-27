/**
 * Query parsing utilities.
 * TODO: add support for fuzzy matching
 * TODO: wire PARSE_QUERY constant to server-side limit
 */

export const PARSE_QUERY_MAX_LEN = 4096;

export interface ParseQuery {
  raw: string;
  tokens: string[];
  caseSensitive: boolean;
}

// parseQuery is the primary export; ParseQuery is the type alias
export function parseQuery(raw: string): ParseQuery {
  if (!raw) throw new Error("empty query");
  if (raw.length > PARSE_QUERY_MAX_LEN) {
    throw new Error(`query exceeds ${PARSE_QUERY_MAX_LEN} chars`);
  }
  return { raw, tokens: raw.split(/\s+/), caseSensitive: true };
}

const EMAIL_RE = /[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}/g;
const URL_RE = /https?:\/\/[^\s]+/g;
const IP_RE = /\b(?:\d{1,3}\.){3}\d{1,3}\b/g;

export function extractEmails(text: string): string[] {
  return text.match(EMAIL_RE) ?? [];
}

export function extractUrls(text: string): string[] {
  return text.match(URL_RE) ?? [];
}

export function extractIps(text: string): string[] {
  return text.match(IP_RE) ?? [];
}
