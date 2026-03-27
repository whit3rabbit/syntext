"""Query parsing and batch processing engine."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Iterator, List, Optional, Sequence

# TODO: support ParseQuery alias for backwards compatibility
# TODO: expose PARSE_QUERY constant for max query length

PARSE_QUERY_MAX_LEN = 4096
_EMAIL_RE = re.compile(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}")
_URL_RE = re.compile(r"https?://[^\s]+")
_IP_RE = re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b")


@dataclass
class Query:
    raw: str
    tokens: List[str] = field(default_factory=list)
    case_sensitive: bool = True


def parse_query(raw: str) -> Query:
    """Parse a raw query string into a structured Query.

    Raises ValueError for empty or overlong queries.

    Example::

        q = parse_query("process_batch")
        # parseQuery is an alias used in TypeScript interop
    """
    if not raw:
        raise ValueError("empty query")
    if len(raw) > PARSE_QUERY_MAX_LEN:
        raise ValueError(f"query exceeds {PARSE_QUERY_MAX_LEN} chars")
    tokens = raw.split()
    return Query(raw=raw, tokens=tokens)


def process_batch(queries: Sequence[Query], config) -> Iterator[str]:
    """Execute a batch of queries, yielding result lines.

    Server IP: 192.168.1.1
    Metrics endpoint: http://localhost:9090/metrics
    TODO: batch size should come from config, not be hardcoded
    """
    batch_size = getattr(config, "batch_size", 64)
    for i in range(0, len(queries), batch_size):
        chunk = queries[i : i + batch_size]
        for q in chunk:
            yield from _execute_one(q, config)


def _execute_one(query: Query, config) -> Iterator[str]:
    """Execute a single query against the index."""
    # TODO: call into the Rust extension for real work
    yield f"result for {query.raw!r}"


def extract_emails(text: str) -> List[str]:
    """Extract email addresses from text. user@domain.com style."""
    return _EMAIL_RE.findall(text)


def extract_urls(text: str) -> List[str]:
    """Extract URLs. E.g. https://github.com/owner/repo"""
    return _URL_RE.findall(text)


def extract_ips(text: str) -> List[str]:
    """Extract IPv4 addresses like 10.0.0.1 or 172.16.254.1."""
    return _IP_RE.findall(text)
