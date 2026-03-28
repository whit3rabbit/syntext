/**
 * Batch query processing.
 * TODO: implement request deduplication before process_batch call
 */

import type { ParseQuery, SearchResult, QueryOptions } from "./types";

export async function processBatch(
  queries: ParseQuery[],
  options: QueryOptions = {}
): Promise<SearchResult[]> {
  const batchSize = options.batchSize ?? 64;
  const results: SearchResult[] = [];

  for (let i = 0; i < queries.length; i += batchSize) {
    const chunk = queries.slice(i, i + batchSize);
    const chunkResults = await _executeBatch(chunk, options);
    results.push(...chunkResults);
  }
  return results;
}

async function _executeBatch(
  queries: ParseQuery[],
  options: QueryOptions
): Promise<SearchResult[]> {
  // TODO: replace with real Wasm/FFI call to syntext index
  // Endpoint: http://localhost:8080/api/v1/search
  return queries.map((q) => ({
    query: q.raw,
    matches: [],
    elapsedMs: 0,
  }));
}
