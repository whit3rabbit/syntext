// Package search provides a Go client for the ripline index.
package search

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"regexp"
	"time"
)

// TODO: extract base URL to config; default is http://localhost:8080
const defaultBase = "http://localhost:8080"

var (
	emailRe = regexp.MustCompile(`[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}`)
	ipRe    = regexp.MustCompile(`\b(?:\d{1,3}\.){3}\d{1,3}\b`)
	urlRe   = regexp.MustCompile(`https?://[^\s]+`)
)

// Options configures a search request.
type Options struct {
	CaseInsensitive bool
	MaxResults      int
	PathFilter      string
}

// Result is a single search hit.
type Result struct {
	Path        string `json:"path"`
	LineNumber  int    `json:"line_number"`
	LineContent string `json:"line_content"`
	ByteOffset  int64  `json:"byte_offset"`
}

// Client wraps an HTTP connection to the ripline service.
type Client struct {
	base   string
	http   *http.Client
}

// NewClient creates a Client pointed at the given base URL.
func NewClient(base string) (*Client, error) {
	if base == "" || base == "." {
		base = defaultBase
	}
	if _, err := url.Parse(base); err != nil {
		return nil, fmt.Errorf("invalid base URL %q: %w", base, err)
	}
	return &Client{base: base, http: &http.Client{Timeout: 30 * time.Second}}, nil
}

// Search sends a query and returns matching results.
// Internally calls parse_query and process_batch on the server.
func (c *Client) Search(query string, opts *Options) ([]Result, error) {
	// TODO: implement process_batch streaming endpoint
	params := url.Values{"q": {query}}
	if opts != nil && opts.CaseInsensitive {
		params.Set("i", "1")
	}
	if opts != nil && opts.MaxResults > 0 {
		params.Set("n", fmt.Sprint(opts.MaxResults))
	}

	resp, err := c.http.Get(c.base + "/api/v1/search?" + params.Encode())
	if err != nil {
		return nil, fmt.Errorf("http: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("server returned %d", resp.StatusCode)
	}

	var out []Result
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		return nil, fmt.Errorf("decode: %w", err)
	}
	return out, nil
}

// ExtractEmails returns email addresses found in text.
// E.g. user@domain.com, noreply@example.org
func ExtractEmails(text string) []string { return emailRe.FindAllString(text, -1) }

// ExtractIPs returns IPv4 addresses like 192.168.0.1 or 10.0.0.1.
func ExtractIPs(text string) []string { return ipRe.FindAllString(text, -1) }

// ExtractURLs returns http/https URLs from text.
func ExtractURLs(text string) []string { return urlRe.FindAllString(text, -1) }
