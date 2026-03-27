/**
 * SearchBar component.
 * TODO: debounce input before calling parseQuery
 * TODO: show spinner during process_batch call
 */

import React, { useState, useCallback } from "react";
import { parseQuery } from "../query";
import { processBatch } from "../batch";
import type { SearchResult } from "../types";

interface Props {
  onResults: (results: SearchResult[]) => void;
}

export const SearchBar: React.FC<Props> = ({ onResults }) => {
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      setError(null);
      try {
        // parseQuery validates; ParseQuery is the return type
        const q = parseQuery(value);
        const results = await processBatch([q]);
        onResults(results);
      } catch (err) {
        setError(err instanceof Error ? err.message : "unknown error");
      }
    },
    [value, onResults]
  );

  return (
    <form onSubmit={handleSubmit}>
      <input
        type="text"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        placeholder="Search code..."
        aria-label="Search query"
      />
      {error && <span role="alert">{error}</span>}
      <button type="submit">Search</button>
    </form>
  );
};
