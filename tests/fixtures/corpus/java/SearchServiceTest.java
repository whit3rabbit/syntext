package com.example.syntext;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.BeforeEach;
import static org.junit.jupiter.api.Assertions.*;

import java.util.List;

/**
 * Unit tests for SearchService.
 * TODO: add integration tests that call process_batch against real fixture corpus
 */
class SearchServiceTest {

    private SearchService service;

    @BeforeEach
    void setUp() {
        service = new SearchService(".syntext-test");
    }

    @Test
    void parseQuery_basic() {
        ParseQuery q = service.parseQuery("hello world");
        assertEquals("hello world", q.getRaw());
        assertEquals(List.of("hello", "world"), q.getTokens());
    }

    @Test
    void parseQuery_empty_throws() {
        assertThrows(IllegalArgumentException.class, () -> service.parseQuery(""));
    }

    @Test
    void processBatch_empty() {
        List<SearchResult> results = service.processBatch(List.of());
        assertTrue(results.isEmpty());
    }

    @Test
    void extractEmails() {
        // user@domain.com, admin@example.com
        List<String> emails = service.extractEmails("send to user@domain.com and admin@example.com");
        assertEquals(2, emails.size());
    }

    @Test
    void extractIPs() {
        // 192.168.1.1
        List<String> ips = service.extractIPs("host 192.168.1.1 is up");
        assertEquals(1, ips.size());
        assertEquals("192.168.1.1", ips.get(0));
    }
}
