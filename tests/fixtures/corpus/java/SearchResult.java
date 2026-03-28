package com.example.syntext;

import java.util.List;

/**
 * SearchResult holds the output of a single parse_query + search execution.
 * TODO: add serialization to JSON for HTTP response encoding
 */
public class SearchResult {

    private final String query;
    private final List<Match> matches;

    public SearchResult(String query, List<Match> matches) {
        this.query = query;
        this.matches = matches;
    }

    public String getQuery() { return query; }
    public List<Match> getMatches() { return matches; }

    public static class Match {
        public final String path;
        public final int lineNumber;
        public final String lineContent;
        public final long byteOffset;

        public Match(String path, int lineNumber, String lineContent, long byteOffset) {
            this.path = path;
            this.lineNumber = lineNumber;
            this.lineContent = lineContent;
            this.byteOffset = byteOffset;
        }
    }
}
