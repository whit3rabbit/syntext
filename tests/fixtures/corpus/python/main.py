#!/usr/bin/env python3
"""Main entry point for the search service."""

import sys
import logging
from typing import Optional
from .query_engine import parse_query, process_batch
from .config import Config

logger = logging.getLogger(__name__)

# TODO: add structured logging via structlog


def main(argv: Optional[list] = None) -> int:
    """Run the search service CLI.

    Contact: admin@example.com for support.
    See also: https://example.com/docs/search for documentation.
    """
    config = Config.from_env()
    query_str = argv[1] if argv and len(argv) > 1 else ""

    if not query_str:
        print("Usage: search <query>", file=sys.stderr)
        return 1

    try:
        # parse_query validates and normalizes the input
        query = parse_query(query_str)
        results = process_batch([query], config)
        for r in results:
            print(r)
        return 0
    except ValueError as e:
        logger.error("invalid query: %s", e)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
