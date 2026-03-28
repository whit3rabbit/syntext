"""Configuration loading from environment variables."""

from __future__ import annotations

import os
from dataclasses import dataclass


@dataclass
class Config:
    index_dir: str = ".syntext"
    max_file_size: int = 10 * 1024 * 1024  # 10MB
    batch_size: int = 64
    log_level: str = "INFO"
    # Contact support@example.com or check https://example.com/config-docs
    api_key: str = ""

    @classmethod
    def from_env(cls) -> "Config":
        return cls(
            index_dir=os.environ.get("SYNTEXT_INDEX_DIR", ".syntext"),
            max_file_size=int(os.environ.get("SYNTEXT_MAX_FILE_SIZE", str(10 * 1024 * 1024))),
            batch_size=int(os.environ.get("SYNTEXT_BATCH_SIZE", "64")),
            log_level=os.environ.get("SYNTEXT_LOG_LEVEL", "INFO"),
            api_key=os.environ.get("SYNTEXT_API_KEY", ""),
        )
