import XCTest
@testable import Syntext

final class SyntextIndexTests: XCTestCase {
    private var tmp: URL!

    override func setUpWithError() throws {
        tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("syntext-swift-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: tmp)
    }

    private func makeRepo(_ files: [String: String]) throws -> URL {
        let repo = tmp.appendingPathComponent("repo")
        try FileManager.default.createDirectory(at: repo, withIntermediateDirectories: true)
        for (name, content) in files {
            let url = repo.appendingPathComponent(name)
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try content.write(to: url, atomically: true, encoding: .utf8)
        }
        return repo
    }

    func testBuildSearchRoundTrip() throws {
        let repo = try makeRepo([
            "a.rs": "fn main() { needle_here(); }\n",
            "b.txt": "nothing to see\n",
        ])
        let idx = try SyntextIndex.build(
            indexDir: tmp.appendingPathComponent("idx").path,
            repoRoot: repo.path)

        let matches = try idx.search("needle_here")
        XCTAssertEqual(matches.count, 1)
        let m = matches[0]
        XCTAssertEqual(m.path, "a.rs")
        XCTAssertEqual(m.lineNumber, 1)
        XCTAssertEqual(m.matchText(), "needle_here")
        XCTAssertEqual(m.lineContentBytes, Data("fn main() { needle_here(); }".utf8))
        XCTAssertEqual(m.submatchStart, 12)
        XCTAssertEqual(m.submatchEnd, 23)

        // No false positive from the other file.
        XCTAssertTrue(try idx.search("nothing_at_all").isEmpty)

        // Stats decode.
        let stats = try idx.stats()
        XCTAssertEqual(stats.totalDocuments, 2)
    }

    func testOpenExistingIndex() throws {
        let repo = try makeRepo(["a.rs": "openable needle\n"])
        let idxDir = tmp.appendingPathComponent("idx")
        _ = try SyntextIndex.build(indexDir: idxDir.path, repoRoot: repo.path)

        let reopened = try SyntextIndex(indexDir: idxDir.path, repoRoot: repo.path)
        XCTAssertEqual(try reopened.search("openable").count, 1)
    }

    func testOpenMissingIndexThrowsCode2() throws {
        let repo = try makeRepo(["a.rs": "x\n"])
        XCTAssertThrowsError(try SyntextIndex(
            indexDir: tmp.appendingPathComponent("missing").path,
            repoRoot: repo.path)) { error in
            guard case SyntextError.indexError(let code, _) = error else {
                return XCTFail("expected indexError, got \(error)")
            }
            XCTAssertEqual(code, 2)
        }
    }

    func testNotifyChangeVisibleAfterCommitBatch() throws {
        // Two base files so one overlay doc stays under the 50% overlay cap
        // (commit_batch enforces OverlayFull above 50% of base docs).
        let repo = try makeRepo([
            "a.rs": "first\n",
            "b.txt": "second\n",
        ])
        let idx = try SyntextIndex.build(
            indexDir: tmp.appendingPathComponent("idx").path, repoRoot: repo.path)

        let newFile = repo.appendingPathComponent("c.rs")
        try "brand_new_needle\n".write(to: newFile, atomically: true, encoding: .utf8)
        // notifyChange takes an absolute path (or one resolvable from cwd);
        // bare names are not resolved against repoRoot.
        try idx.notifyChange(newFile.path)
        XCTAssertTrue(try idx.search("brand_new_needle").isEmpty, "visible before commit")

        try idx.commitBatch()
        XCTAssertEqual(try idx.search("brand_new_needle").count, 1, "missing after commit")
    }

    func testSearchFreshOnNonGitDirIsNoChanges() throws {
        let repo = try makeRepo(["a.rs": "fresh needle\n"])
        let idx = try SyntextIndex.build(
            indexDir: tmp.appendingPathComponent("idx").path, repoRoot: repo.path)

        let result = try idx.searchFresh("fresh needle")
        XCTAssertEqual(result.matches.count, 1)
        guard case .noChanges = result.outcome else {
            return XCTFail("expected noChanges, got \(result.outcome)")
        }
    }

    func testExplicitMaxResultsHonored() throws {
        let repo = try makeRepo(["lines.txt": (0..<50).map { "hit line \($0)" }.joined(separator: "\n") + "\n"])
        let idx = try SyntextIndex.build(
            indexDir: tmp.appendingPathComponent("idx").path, repoRoot: repo.path)
        XCTAssertEqual(try idx.search("hit").count, 50)
        XCTAssertEqual(
            try idx.search("hit", options: SyntextSearchOptions(maxResults: 5)).count, 5)
    }

    func testContextLines() throws {
        let repo = try makeRepo([
            "lines.txt": "alpha\nbravo\ncharlie needle delta\necho\nfoxtrot\n"
        ])
        let idx = try SyntextIndex.build(
            indexDir: tmp.appendingPathComponent("idx").path, repoRoot: repo.path)

        // Context 1 line before, 2 lines after
        let matches = try idx.search(
            "needle",
            options: SyntextSearchOptions(beforeContext: 1, afterContext: 2)
        )
        XCTAssertEqual(matches.count, 1)
        let m = matches[0]
        XCTAssertEqual(m.lineNumber, 3)
        XCTAssertEqual(m.context.count, 4)
        XCTAssertEqual(m.beforeContext.map(\.lineContent), ["bravo"])
        XCTAssertEqual(m.beforeContext.map(\.lineNumber), [2])
        XCTAssertEqual(m.afterContext.map(\.lineContent), ["echo", "foxtrot"])
        XCTAssertEqual(m.afterContext.map(\.lineNumber), [4, 5])
    }

    func testFixedStrings() throws {
        let repo = try makeRepo([
            "code.rs": "let val = map.get(key)[0];\nlet other = 42;\n"
        ])
        let idx = try SyntextIndex.build(
            indexDir: tmp.appendingPathComponent("idx").path, repoRoot: repo.path)

        // "map.get(key)[0]" has unescaped regex metacharacters: ., (, ), [, ]
        let matches = try idx.search(
            "map.get(key)[0]",
            options: SyntextSearchOptions(fixedStrings: true)
        )
        XCTAssertEqual(matches.count, 1)
        XCTAssertEqual(matches[0].lineNumber, 1)
        XCTAssertEqual(matches[0].matchText(), "map.get(key)[0]")
    }

    func testWordRegexp() throws {
        let repo = try makeRepo([
            "words.txt": "account count counter\n"
        ])
        let idx = try SyntextIndex.build(
            indexDir: tmp.appendingPathComponent("idx").path, repoRoot: repo.path)

        let matches = try idx.search(
            "count",
            options: SyntextSearchOptions(wordRegexp: true)
        )
        XCTAssertEqual(matches.count, 1)
        XCTAssertEqual(matches[0].submatchStart, 8)
        XCTAssertEqual(matches[0].matchText(), "count")
    }

    func testFormattedGrepOutput() throws {
        let repo = try makeRepo([
            "sample.txt": "111\n222 match\n333\n"
        ])
        let idx = try SyntextIndex.build(
            indexDir: tmp.appendingPathComponent("idx").path, repoRoot: repo.path)

        let output = try idx.grep("match", options: SyntextSearchOptions(context: 1))
        XCTAssertEqual(output, "sample.txt-1-111\nsample.txt:2:222 match\nsample.txt-3-333")
    }

    func testAsyncSearch() async throws {
        let repo = try makeRepo([
            "async.txt": "hello from async task\n"
        ])
        let idx = try await SyntextIndex.buildAsync(
            indexDir: tmp.appendingPathComponent("idx").path, repoRoot: repo.path)

        let matches = try await idx.searchAsync("async")
        XCTAssertEqual(matches.count, 1)
        XCTAssertEqual(matches[0].lineNumber, 1)

        let grepOut = try await idx.grepAsync("async")
        XCTAssertEqual(grepOut, "async.txt:1:hello from async task")
    }
}

final class SyntextChatIndexTests: XCTestCase {
    func testAddCommitSearchRemove() throws {
        let chats = try SyntextChatIndex()
        try chats.add("chats/1", content: "hello needle one\n")
        try chats.add("chats/2", content: "second needle here\n")

        // Uncommitted: invisible.
        XCTAssertTrue(try chats.search("needle").isEmpty)
        try chats.commit()

        let matches = try chats.search("needle")
        XCTAssertEqual(matches.count, 2)
        XCTAssertEqual(Set(matches.map(\.path)), ["chats/1", "chats/2"])
        XCTAssertEqual(matches[0].matchText(), "needle")

        // Removal applies only after commit.
        try chats.remove("chats/2")
        XCTAssertEqual(try chats.search("needle").count, 2)
        try chats.commit()
        XCTAssertEqual(try chats.search("needle").count, 1)
    }

    func testTraversalIdsRejectedWithCode6() throws {
        let chats = try SyntextChatIndex()
        for bad in ["../x", "/abs", "a/../b", ""] {
            XCTAssertThrowsError(try chats.add(bad, content: "m")) { error in
                guard case SyntextError.indexError(let code, _) = error else {
                    return XCTFail("expected indexError for \(bad), got \(error)")
                }
                XCTAssertEqual(code, 6, "id \(bad)")
            }
        }
    }

    func testNonUTF8ContentRoundTripsByteExact() throws {
        let chats = try SyntextChatIndex()
        // Invalid UTF-8 without a BOM or NUL: indexed, exact bytes preserved.
        try chats.add("raw/1", content: Data([0x6e, 0x61, 0xFF, 0x76, 0x65, 0x20, 0x6e, 0x65, 0x65, 0x64, 0x6c, 0x65, 0x0a]))
        try chats.commit()

        let matches = try chats.search("needle")
        XCTAssertEqual(matches.count, 1)
        // "na\xFFve needle": needle occupies bytes 6..<12 of the exact bytes.
        XCTAssertEqual(matches[0].submatchStart, 6)
        XCTAssertEqual(matches[0].submatchEnd, 12)
        XCTAssertEqual(matches[0].matchText(), "needle")
        XCTAssertEqual(matches[0].lineContentBytes[2], 0xFF)
        XCTAssertTrue(matches[0].lineContent.contains("\u{FFFD}"))
    }

    func testDefaultMaxResultsCap() throws {
        let chats = try SyntextChatIndex()
        let line = Data("hit\n".utf8)
        try chats.add("big", content: line + line + line)  // 3 lines is enough to
        try chats.commit()                                  // prove the default applies
        XCTAssertFalse(try chats.search("hit").isEmpty)
    }

    func testVersionReportsLinkedLibrary() {
        XCTAssertFalse(SyntextFFI.version.isEmpty)
    }
}
