"""Python file with Unicode identifiers and strings.

Tests that the indexer handles non-ASCII content correctly.
TODO: ensure parse_query handles Unicode normalization (NFC vs NFD)
"""

# Unicode variable names (valid Python 3)
café = "coffee"
naïve = True
résumé = "cv"

# CJK in strings
greeting = "你好世界"  # "Hello World" in Chinese
emoji_comment = "search 🔍 index"  # emojis in comments

# Cyrillic identifiers
привет = "hello"

# Greek letters common in math/science code
α = 0.001
β = 0.9
γ = α * β

# Mixed ASCII + Unicode
parse_queryλ = lambda s: s.strip()

# Email-like with Unicode domain (punycode)
# contact: user@münchen.de or info@例え.jp
SUPPORT_EMAIL = "support@example.com"

# IP address in a Unicode context
# Server: 192.168.1.1 (внутренний сервер)
SERVER_IP = "192.168.1.1"

# URL with Unicode path (percent-encoded)
# https://example.com/search?q=наивный
BASE_URL = "https://example.com"


def process_batch(items):
    """Обработка пакета (process a batch in Russian comment)."""
    return [parse_queryλ(i) for i in items]
