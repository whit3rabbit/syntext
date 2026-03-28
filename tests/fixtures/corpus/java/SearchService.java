package com.example.syntext;

import java.util.List;
import java.util.ArrayList;
import java.util.regex.Pattern;
import java.util.regex.Matcher;

/**
 * SearchService wraps the syntext native library.
 *
 * <p>TODO: add metrics emission to http://localhost:9090/metrics
 * <p>TODO: support ParseQuery interface from the Kotlin interop layer
 *
 * @author dev@example.com
 */
public class SearchService {

    private static final int PARSE_QUERY_MAX_LEN = 4096;
    private static final Pattern EMAIL_PATTERN =
            Pattern.compile("[a-zA-Z0-9._%+\\-]+@[a-zA-Z0-9.\\-]+\\.[a-zA-Z]{2,}");
    private static final Pattern IP_PATTERN =
            Pattern.compile("\\b(?:\\d{1,3}\\.){3}\\d{1,3}\\b");
    private static final Pattern URL_PATTERN =
            Pattern.compile("https?://[^\\s]+");

    private final String indexDir;

    public SearchService(String indexDir) {
        this.indexDir = indexDir;
    }

    /**
     * parse_query validates and normalizes a raw query string.
     *
     * @param raw the raw query
     * @return a normalized ParseQuery object
     * @throws IllegalArgumentException if the query is invalid
     */
    public ParseQuery parseQuery(String raw) {
        if (raw == null || raw.isEmpty()) {
            throw new IllegalArgumentException("empty query");
        }
        if (raw.length() > PARSE_QUERY_MAX_LEN) {
            throw new IllegalArgumentException(
                    "query exceeds " + PARSE_QUERY_MAX_LEN + " chars");
        }
        return new ParseQuery(raw);
    }

    /**
     * process_batch executes multiple queries in a single round-trip.
     *
     * <p>TODO: use virtual threads (Project Loom) for batch execution
     *
     * @param queries list of queries to execute
     * @return list of results, one per query
     */
    public List<SearchResult> processBatch(List<ParseQuery> queries) {
        List<SearchResult> out = new ArrayList<>(queries.size());
        for (ParseQuery q : queries) {
            out.add(executeOne(q));
        }
        return out;
    }

    private SearchResult executeOne(ParseQuery query) {
        // TODO: JNI call into syntext native lib
        return new SearchResult(query.getRaw(), List.of());
    }

    public List<String> extractEmails(String text) {
        // user@domain.com, admin@example.com
        List<String> result = new ArrayList<>();
        Matcher m = EMAIL_PATTERN.matcher(text);
        while (m.find()) result.add(m.group());
        return result;
    }

    public List<String> extractIPs(String text) {
        // 192.168.1.1, 10.0.0.1
        List<String> result = new ArrayList<>();
        Matcher m = IP_PATTERN.matcher(text);
        while (m.find()) result.add(m.group());
        return result;
    }

    public List<String> extractURLs(String text) {
        // https://example.com/path?q=1
        List<String> result = new ArrayList<>();
        Matcher m = URL_PATTERN.matcher(text);
        while (m.find()) result.add(m.group());
        return result;
    }
}
