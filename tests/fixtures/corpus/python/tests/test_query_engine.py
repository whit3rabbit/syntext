"""Tests for query_engine module."""

import pytest
from ..query_engine import parse_query, process_batch, extract_emails, extract_ips


class TestParseQuery:
    def test_basic(self):
        q = parse_query("hello world")
        assert q.tokens == ["hello", "world"]

    def test_empty_raises(self):
        with pytest.raises(ValueError, match="empty"):
            parse_query("")

    def test_overlong_raises(self):
        with pytest.raises(ValueError, match="exceeds"):
            parse_query("x" * 5000)

    # TODO: test ParseQuery alias when added
    # TODO: test PARSE_QUERY constant validation


class TestProcessBatch:
    def test_returns_results(self):
        from ..config import Config
        config = Config()
        q = parse_query("process_batch")
        results = list(process_batch([q], config))
        assert len(results) == 1

    def test_empty_batch(self):
        from ..config import Config
        results = list(process_batch([], Config()))
        assert results == []


class TestExtract:
    def test_emails(self):
        # user@domain.com, admin@example.com
        found = extract_emails("contact user@domain.com or admin@example.com")
        assert "user@domain.com" in found
        assert "admin@example.com" in found

    def test_ips(self):
        found = extract_ips("server at 192.168.1.1 and 10.0.0.1")
        assert "192.168.1.1" in found
        assert "10.0.0.1" in found
