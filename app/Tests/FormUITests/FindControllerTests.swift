import FormCore
import Testing

@testable import FormUI

/// `⌘F` (spec 14 §4, §6): all matches found, `n of m`, stepping and wrapping, the toggles,
/// and surviving a streaming update without losing the current match.
@MainActor
struct FindControllerTests {
    private func openFind(_ harness: CommandsHarness, query: String = "a") async -> FindController {
        harness.center.openFind(seed: query)
        await harness.wait { harness.center.find.hasMatches }
        return harness.center.find
    }

    @Test("opening with a selection seeds the query")
    func seedsFromSelection() async {
        let harness = CommandsHarness()
        harness.center.hooks.selectedText = { "ring" }
        await harness.center.run(id: "find.open")
        #expect(harness.center.find.query == "ring")
        #expect(harness.center.isPresented(.find))
    }

    @Test("a multi-line selection is not used as a query")
    func ignoresMultilineSelection() {
        let harness = CommandsHarness()
        harness.center.openFind(seed: "two\nlines")
        #expect(harness.center.find.query.isEmpty)
    }

    @Test("matches come back with the core's ranges")
    func matchesUseCoreRanges() async throws {
        let harness = CommandsHarness()
        let find = await openFind(harness, query: "ring")
        let match = try #require(find.matches.first)
        let range = try #require(match.ranges.first)
        let resolved = try #require(range.range(in: match.snippet))
        #expect(String(match.snippet[resolved]).lowercased() == "ring")
    }

    @Test("n of m counts, 1-based")
    func positionLabel() async {
        let harness = CommandsHarness()
        let find = await openFind(harness)
        #expect(find.matches.count > 1)
        #expect(find.positionLabel == "1 of \(find.matches.count)")
        find.next()
        #expect(find.positionLabel == "2 of \(find.matches.count)")
    }

    @Test("stepping wraps at both ends and reports which edge")
    func steppingWraps() async {
        let harness = CommandsHarness()
        let find = await openFind(harness)
        let count = find.matches.count
        #expect(count > 1)

        for _ in 0..<(count - 1) { find.next() }
        #expect(find.currentIndex == count - 1)
        #expect(find.wrapped == nil)

        find.next()
        #expect(find.currentIndex == 0, "past the last match wraps to the first")
        #expect(find.wrapped == .end)

        find.clearWrap()
        find.previous()
        #expect(find.currentIndex == count - 1, "before the first match wraps to the last")
        #expect(find.wrapped == .start)
    }

    @Test("stepping publishes a scroll target and a flash")
    func steppingScrolls() async {
        let harness = CommandsHarness()
        let find = await openFind(harness)
        let before = find.flashToken
        find.next()
        #expect(find.flashToken > before)
        #expect(find.scrollTarget?.entryId == find.current?.entryId)

        // Stepping onto the same entry twice still moves: the token changes every time.
        let token = find.scrollTarget?.token
        find.previous()
        #expect(find.scrollTarget?.token != token)
    }

    @Test("stepping does nothing when there are no matches")
    func steppingIsSafeWhenEmpty() {
        let harness = CommandsHarness()
        harness.center.openFind(seed: nil)
        harness.center.find.next()
        harness.center.find.previous()
        #expect(harness.center.find.currentIndex == 0)
        #expect(harness.center.find.positionLabel.isEmpty)
    }

    @Test("case-sensitive narrows the core's hits without re-searching")
    func caseSensitiveFilters() async {
        let harness = CommandsHarness()
        let find = await openFind(harness, query: "A")
        let insensitive = find.matches.count
        #expect(insensitive > 1)

        find.caseSensitive = true
        #expect(find.matches.count < insensitive)
        // Everything left really does match the query's case.
        for match in find.matches {
            let matched = match.ranges.compactMap { $0.range(in: match.snippet) }
                .map { String(match.snippet[$0]) }
            #expect(matched.contains("A"))
        }

        find.caseSensitive = false
        #expect(find.matches.count == insensitive, "clearing the toggle restores the hits")
    }

    @Test("whole-word narrows the core's hits")
    func wholeWordFilters() async {
        let harness = CommandsHarness()
        let find = await openFind(harness, query: "a")
        let loose = find.matches.count
        find.wholeWord = true
        #expect(find.matches.count < loose)
    }

    @Test("a streaming update does not lose the current match")
    func survivesAStreamingUpdate() async throws {
        let harness = CommandsHarness()
        let find = await openFind(harness)
        #expect(find.matches.count > 2)

        find.next()
        find.next()
        let anchored = try #require(find.current)
        #expect(find.currentIndex == 2)

        // What `CommandsOverlay` does on every transcript change while a run streams.
        find.refresh()
        await harness.wait { !find.isSearching }

        #expect(find.current?.id == anchored.id, "the current match moved")
        #expect(find.currentIndex == 2)
    }

    @Test("closing clears the query, the matches and the highlights")
    func closingClears() async {
        let harness = CommandsHarness()
        let find = await openFind(harness)
        #expect(find.hasMatches)

        harness.center.dismiss(.find)
        #expect(!harness.center.isPresented(.find))
        #expect(!harness.state.findPresented)
        #expect(find.matches.isEmpty)
        #expect(find.query.isEmpty)
        #expect(find.scrollTarget == nil)
    }

    @Test("Esc closes the find bar before it stops streaming")
    func escapeClosesFindFirst() async {
        let harness = CommandsHarness(scenario: .streaming)
        harness.center.openFind(seed: "a")
        #expect(await harness.center.handleEscape())
        #expect(!harness.center.isPresented(.find))
        #expect(harness.stores.chat.isStreaming, "streaming was not touched")
    }

    @Test("⌘G and ⌘⇧G step once the bar has matches")
    func commandsStepMatches() async {
        let harness = CommandsHarness()
        let find = await openFind(harness)
        #expect(AppCommands.command(id: "find.next")?.isEnabled(harness.context) == true)

        await harness.center.run(id: "find.next")
        #expect(find.currentIndex == 1)
        await harness.center.run(id: "find.previous")
        #expect(find.currentIndex == 0)
    }

    @Test("highlights are reported per entry, with the current one called out")
    func highlightsPerEntry() async throws {
        let harness = CommandsHarness()
        let find = await openFind(harness)
        let match = try #require(find.current)
        guard let entryId = match.entryId else { return }  // title-only corpus: nothing to check
        let highlights = find.highlights(forEntry: entryId)
        #expect(!highlights.all.isEmpty)
        #expect(highlights.current == match.ranges.first)
    }
}
