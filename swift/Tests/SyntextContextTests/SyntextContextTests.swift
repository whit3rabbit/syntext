import XCTest
import Syntext
@testable import SyntextContext

final class SyntextContextTests: XCTestCase {
    private var tmp: URL!

    override func setUpWithError() throws {
        tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("syntext-context-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: tmp)
    }

    private func makeRepo(_ files: [String: String]) throws -> URL {
        let repo = tmp.appendingPathComponent("repo")
        for (name, content) in files {
            let url = repo.appendingPathComponent(name)
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try content.write(to: url, atomically: true, encoding: .utf8)
        }
        return repo
    }

    private func openStore() throws -> SyntextContextStore {
        try SyntextContextStore(
            directory: tmp.appendingPathComponent("store").path,
            options: .init(sync: false))
    }

    func testSyncAssembleAndCheckSources() throws {
        let repo = try makeRepo([
            "README.md": "# Billing\n\nFailed charges are retried three times, unless the card was reported stolen.\n",
            "src/retry.rs": "/// Retry a failed charge.\npub fn should_retry(attempt: u32) -> bool {\n    attempt < 3\n}\n",
        ])
        let store = try openStore()
        let source = SyntextDirectorySource(label: "repo", root: repo.path)
        var calls = 0
        let report = try store.sync(source) { progress in
            calls += 1
            XCTAssertLessThanOrEqual(progress.observed, progress.total)
            return true
        }
        XCTAssertEqual(report.added, 2)
        XCTAssertEqual(calls, 1)

        let engine = try SyntextContextEngine(store: store)
        let turn = try engine.assemble(
            SyntextChatRequest(question: "Are failed charges retried?", window: 4096, system: "Be accurate."))
        XCTAssertEqual(turn.messages.first?.role, .system)
        XCTAssertFalse(turn.messages.first!.content.contains("<evidence id="), "evidence never in the system role")
        let user = try XCTUnwrap(turn.messages.last)
        XCTAssertEqual(user.role, .user)
        XCTAssertTrue(user.content.contains("reported stolen"))
        XCTAssertTrue(user.content.hasSuffix("Are failed charges retried?"))
        XCTAssertLessThanOrEqual(turn.promptTokens, 4096 - turn.budget.generation)
        XCTAssertEqual(turn.packet.lowestTrust, .workspace)

        try "# Billing\n\nRewritten.\n".write(
            to: repo.appendingPathComponent("README.md"), atomically: true, encoding: .utf8)
        let checks = try SyntextContextEngine.checkSources(
            receiptJSON: turn.packet.receiptJSON, root: repo.path, label: "repo")
        XCTAssertTrue(checks.contains { $0.status == .changed })
    }

    func testSearchFindsTheDefinition() throws {
        let repo = try makeRepo([
            "src/retry.rs": "/// Retry a failed charge.\npub fn should_retry(attempt: u32) -> bool {\n    attempt < 3\n}\n",
        ])
        let store = try openStore()
        try store.sync(SyntextDirectorySource(label: "repo", root: repo.path))
        let engine = try SyntextContextEngine(store: store)
        let found = try engine.search("where is should_retry defined", maxResults: 3)
        XCTAssertEqual(found.first?.source, "src/retry.rs")
        XCTAssertEqual(found.first?.symbol, "should_retry")
    }

    func testCancelledSyncPublishesNothing() throws {
        var files: [String: String] = [:]
        for i in 0..<5 { files["n\(i).md"] = "note \(i)\n" }
        let repo = try makeRepo(files)
        let store = try openStore()
        XCTAssertThrowsError(
            try store.sync(SyntextDirectorySource(label: "repo", root: repo.path)) { _ in false }
        ) { error in
            XCTAssertTrue((error as? SyntextError)?.isCancelled == true, "\(error)")
        }
        XCTAssertEqual(try store.stats().documents, 0)
    }

    func testConversationMemoryAndHostTokenCounter() throws {
        let store = try openStore()
        try store.upsert([SyntextContextDocument(id: "policy", text: "Refunds are issued within 45 days of purchase.")])
        let chat = try SyntextConversation(store: store, session: "chat-1", mode: .project("billing"))
        try chat.recordExchange(question: "I bought it on March 3rd.", answer: "Noted.")
        XCTAssertEqual(try chat.history().count, 2)

        let engine = try SyntextContextEngine(
            store: store,
            options: .init(model: "test-model", tokenizer: "words"),
            tokenCounter: { $0.split(whereSeparator: \.isWhitespace).count })
        let turn = try engine.assemble(
            SyntextChatRequest(question: "When do refunds expire?", window: 1024), conversation: chat)
        XCTAssertEqual(turn.messages.first(where: { $0.role == .user })?.content, "I bought it on March 3rd.")
        XCTAssertTrue(turn.messages.last!.content.contains("45 days"))
        XCTAssertEqual(turn.packet.receipt.tokenizer, "words")
        XCTAssertEqual(turn.packet.receipt.model, "test-model")

        // Resumed from disk by a second handle.
        let again = try SyntextConversation(store: store, session: "chat-1", mode: .project("billing"))
        XCTAssertEqual(try again.history().map(\.content), ["I bought it on March 3rd.", "Noted."])
    }

    func testPrepareRecallsOlderTurnsForHostOwnedPrompts() throws {
        let store = try openStore()
        let chat = try SyntextConversation(store: store, session: "chat-2", mode: .app("notes"))
        try chat.recordExchange(question: "My invoice number is INV-7731.", answer: "Noted.")
        try chat.recordExchange(question: "Which invoice did I mention?", answer: "INV-7731.")
        let engine = try SyntextContextEngine(store: store)
        let question = "What was my invoice number?"

        let plain = try engine.prepare(question: question, evidenceTokens: 400)
        XCTAssertFalse(plain.evidenceText.contains("INV-7731"), "memory scope is not granted by default")

        let packet = try engine.prepare(
            question: question, evidenceTokens: 400, conversation: chat, keptTurns: 2)
        XCTAssertTrue(packet.evidenceText.contains("My invoice number is INV-7731."))
        XCTAssertFalse(packet.evidenceText.contains("Which invoice did I mention?"))
        XCTAssertEqual(packet.lowestTrust, .memory)
    }

    func testEmbeddingsFindAParaphrase() throws {
        let store = try openStore()
        try store.upsert([
            SyntextContextDocument(id: "refunds", text: "Refunds are issued within 45 days of purchase."),
            SyntextContextDocument(id: "menu", text: "The cafeteria menu changes every Monday."),
        ])
        // A toy model: "money" and "refunds" share a dimension; every other
        // word gets one of 62 others from a stable hash (not `hashValue`,
        // which Swift seeds per process), so unrelated words rarely meet.
        let embedder = try SyntextContextEmbedder(id: "toy-v1", dimensions: 64) { texts, _ in
            texts.map { text in
                var v = [Float](repeating: 0, count: 64)
                v[63] = 0.01
                for word in text.lowercased().split(whereSeparator: { !$0.isLetter }) where word.count > 2 {
                    let w = String(word)
                    let h = w.unicodeScalars.reduce(UInt32(7)) { $0 &* 31 &+ $1.value }
                    let d = (w == "money" || w.hasPrefix("refund")) ? 0 : 1 + Int(h % 62)
                    v[d] += 1
                }
                return v
            }
        }
        var batches = 0
        let report = try store.embed(embedder, batch: 1) { _ in
            batches += 1
            return true
        }
        XCTAssertEqual(report.embedded, 2)
        XCTAssertEqual(report.remaining, 0)
        XCTAssertEqual(batches, 2)

        let engine = try SyntextContextEngine(store: store, embedder: embedder)
        let packet = try engine.prepare(question: "How do I get my money back?", evidenceTokens: 300)
        XCTAssertTrue(packet.evidenceText.contains("Refunds are issued"))

        let failing = try SyntextContextEmbedder(id: "broken", dimensions: 64) { _, _ in [] }
        XCTAssertThrowsError(try store.embed(failing)) { error in
            XCTAssertTrue((error as? SyntextError)?.isEmbedderFailure == true, "\(error)")
        }
    }

    func testOversizedQuestionIsAnError() throws {
        let store = try openStore()
        let engine = try SyntextContextEngine(store: store)
        let huge = String(repeating: "word ", count: 5_000)
        XCTAssertThrowsError(try engine.assemble(SyntextChatRequest(question: huge, window: 1024))) { error in
            XCTAssertTrue((error as? SyntextError)?.isWindowExceeded == true, "\(error)")
        }
    }

    func testConstantSystemPromptWithThePolicyIncluded() throws {
        let store = try openStore()
        try store.upsert([SyntextContextDocument(id: "a", text: "The ingest worker restarts nightly.")])
        let engine = try SyntextContextEngine(store: store, options: .init(appendPolicy: false))
        let system = "You are helpful.\n\n" + SyntextContextEngine.evidencePolicy
        let turn = try engine.assemble(
            SyntextChatRequest(question: "When does the ingest worker restart?", window: 2048, system: system))
        XCTAssertEqual(turn.messages.first?.content, system)
        XCTAssertFalse(turn.packet.isEmpty)
    }

    func testSeparateMessagePlacement() throws {
        let store = try openStore()
        try store.upsert([SyntextContextDocument(id: "a", text: "The ingest worker restarts nightly.")])
        let engine = try SyntextContextEngine(
            store: store, options: .init(order: .document, placement: .separateMessage))
        let question = "When does the ingest worker restart?"
        let turn = try engine.assemble(SyntextChatRequest(question: question, window: 2048))
        let users = turn.messages.filter { $0.role == .user }
        XCTAssertEqual(users.count, 2)
        XCTAssertTrue(users[0].content.contains("<evidence id=\"E1\""))
        XCTAssertEqual(users[1].content, question)
    }

    func testPathSyncRemoveCompactAndReadOnlyReload() throws {
        let repo = try makeRepo(["a.md": "# A\n\nalpha\n", "b.md": "# B\n\nbeta\n"])
        let storeDir = tmp.appendingPathComponent("store").path
        let store = try SyntextContextStore(directory: storeDir, options: .init(sync: false))
        let source = SyntextDirectorySource(label: "repo", root: repo.path)
        try store.sync(source)

        try "# A\n\nalpha changed\n".write(
            to: repo.appendingPathComponent("a.md"), atomically: true, encoding: .utf8)
        let report = try store.sync(paths: [repo.appendingPathComponent("a.md").path], in: source)
        XCTAssertEqual(report.modified, 1)

        let removed = try store.remove(keys: ["repo:b.md", "repo:missing.md"])
        XCTAssertEqual(removed.removed, 1)
        let compacted = try XCTUnwrap(try store.compact(force: true))
        XCTAssertEqual(compacted.documents, 1)
        XCTAssertEqual(try store.stats().documents, 1)

        let reader = try SyntextContextStore(directory: storeDir, options: .init(readOnly: true))
        XCTAssertFalse(try reader.reload(), "nothing new since open")
        try store.upsert([SyntextContextDocument(id: "n", text: "a new note")])
        XCTAssertTrue(try reader.reload())
        XCTAssertEqual(try reader.stats().documents, 2)
    }

    func testAsyncPrepareAndVersion() async throws {
        let store = try openStore()
        try store.upsert([SyntextContextDocument(id: "a", text: "The ingest worker restarts nightly.")])
        let engine = try SyntextContextEngine(store: store)
        let packet = try await engine.prepareAsync(question: "When does the ingest worker restart?", evidenceTokens: 400)
        XCTAssertFalse(packet.isEmpty)
        XCTAssertTrue(packet.evidenceText.contains("nightly"))
        XCTAssertFalse(SyntextContextFFI.version.isEmpty)
    }
}
