#!/usr/bin/env python3
import sys
import re
import os

def main():
    if len(sys.argv) < 2:
        print("Usage: extract_release_notes.py <version> [changelog_path]", file=sys.stderr)
        sys.exit(1)
        
    version = sys.argv[1]
    if version.startswith('v'):
        version = version[1:]
        
    changelog_path = sys.argv[2] if len(sys.argv) > 2 else "CHANGELOG.md"
    
    if not os.path.exists(changelog_path):
        print(f"Error: {changelog_path} not found.", file=sys.stderr)
        sys.exit(1)
        
    with open(changelog_path, 'r', encoding='utf-8') as f:
        content = f.read()
        
    # Match heading like: ## [1.4.0] - 2026-06-13 or ## [1.4.0] or ## 1.4.0
    # The header has optional brackets around the version.
    pattern = r'(?m)^##\s+\[?' + re.escape(version) + r'\]?(?:\s+-\s+\d{4}-\d{2}-\d{2})?\s*$'
    match = re.search(pattern, content)
    if not match:
        print(f"Warning: Version {version} not found in {changelog_path}", file=sys.stderr)
        sys.exit(0)
        
    start_idx = match.end()
    # Find the next header starting with ##
    next_match = re.search(r'(?m)^##\s+', content[start_idx:])
    if next_match:
        notes = content[start_idx:start_idx+next_match.start()]
    else:
        notes = content[start_idx:]
        
    print(notes.strip())

if __name__ == "__main__":
    main()
