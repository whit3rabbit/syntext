package com.example.syntext;

import java.util.Arrays;
import java.util.List;

/**
 * ParseQuery is a validated, normalized query ready for execution.
 *
 * <p>The class name mirrors the parse_query function in the Rust core so that
 * cross-language search tests can find both parse_query (function) and
 * ParseQuery (class) in a single corpus query.
 *
 * <p>TODO: add support for parseQuery factory method (camelCase alias)
 */
public class ParseQuery {

    private final String raw;
    private final List<String> tokens;
    private final boolean caseSensitive;

    public ParseQuery(String raw) {
        this(raw, true);
    }

    public ParseQuery(String raw, boolean caseSensitive) {
        this.raw = raw;
        this.tokens = Arrays.asList(raw.split("\\s+"));
        this.caseSensitive = caseSensitive;
    }

    public String getRaw() { return raw; }
    public List<String> getTokens() { return tokens; }
    public boolean isCaseSensitive() { return caseSensitive; }

    @Override
    public String toString() {
        return "ParseQuery{raw=" + raw + ", caseSensitive=" + caseSensitive + "}";
    }
}
