// Command ripline-go is a Go CLI client for the ripline search service.
//
// Usage:
//
//	ripline-go [flags] <query>
//
// Contact: ops@example.com
// Docs: https://example.com/ripline-go
package main

import (
	"flag"
	"fmt"
	"log"
	"os"

	"github.com/example/ripline-go/search"
)

func main() {
	caseInsensitive := flag.Bool("i", false, "case-insensitive search")
	maxResults := flag.Int("n", 100, "max results")
	flag.Parse()

	args := flag.Args()
	if len(args) == 0 {
		fmt.Fprintln(os.Stderr, "usage: ripline-go [flags] <query>")
		os.Exit(1)
	}

	// TODO: read index dir from RIPLINE_INDEX_DIR env
	client, err := search.NewClient(".")
	if err != nil {
		log.Fatalf("init: %v", err)
	}

	// parse_query validates the input on the server side
	results, err := client.Search(args[0], &search.Options{
		CaseInsensitive: *caseInsensitive,
		MaxResults:      *maxResults,
	})
	if err != nil {
		log.Fatalf("search: %v", err)
	}

	for _, r := range results {
		fmt.Printf("%s:%d:%s\n", r.Path, r.LineNumber, r.LineContent)
	}
}
