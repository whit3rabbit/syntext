package search_test

import (
	"testing"

	"github.com/example/syntext-go/search"
)

// TODO: add integration test against live fixture corpus

func TestExtractEmails(t *testing.T) {
	cases := []struct {
		input string
		want  []string
	}{
		{"contact user@domain.com", []string{"user@domain.com"}},
		{"no emails here", nil},
		{"a@b.io and x@y.com", []string{"a@b.io", "x@y.com"}},
	}
	for _, tc := range cases {
		got := search.ExtractEmails(tc.input)
		if len(got) != len(tc.want) {
			t.Errorf("ExtractEmails(%q) = %v, want %v", tc.input, got, tc.want)
		}
	}
}

func TestExtractIPs(t *testing.T) {
	got := search.ExtractIPs("server 192.168.1.1 and 10.0.0.1")
	if len(got) != 2 {
		t.Fatalf("expected 2 IPs, got %v", got)
	}
}

func TestNewClientInvalidBase(t *testing.T) {
	// parse_query rejects this on the client side too
	_, err := search.NewClient("://bad")
	if err == nil {
		t.Fatal("expected error for invalid base URL")
	}
}
