import FormCore
import Foundation
import Observation

/// One match the find bar can step to.
///
/// `ranges` are the core's `{start, len}` UTF-16 ranges over `snippet` (spec 01 §4). Nothing
/// here re-searches or re-derives them — the core owns what matched, and Swift owns only
/// where to draw.
public struct FindMatch: Identifiable, Sendable, Equatable {
    public let sessionId: String
    public let entryId: String?
    public let snippet: String
    public let ranges: [HighlightRange]
    public let timestamp: TimestampMs

    /// Identity, not position: this is what the current match is re-anchored by when a
    /// streaming update re-runs the search (spec 14 §6).
    public var id: String { "\(sessionId)#\(entryId ?? "title")#\(ranges.first?.start ?? 0)" }

    init(hit: SearchHit) {
        sessionId = hit.sessionId
        entryId = hit.entryId
        snippet = hit.snippet
        ranges = hit.highlights
        timestamp = hit.timestamp
    }
}

/// `⌘F` — find in the open session (F13.2, spec 14 §4).
///
/// Backed by `searchInSession`, which returns one hit per matching entry with the matched
/// range already computed. The case-sensitive and whole-word toggles **filter those hits**
/// using the supplied ranges rather than running a second search in Swift: FTS5 is
/// case-folded and token-based, so the toggles are a narrowing of what the core found, and
/// the ranges stay the core's.
@MainActor
@Observable
public final class FindController {
    public var query = "" {
        didSet { if query != oldValue { scheduleSearch() } }
    }

    public var caseSensitive = false {
        didSet { if caseSensitive != oldValue { applyFilters() } }
    }

    public var wholeWord = false {
        didSet { if wholeWord != oldValue { applyFilters() } }
    }

    public private(set) var matches: [FindMatch] = []
    public private(set) var currentIndex = 0
    public private(set) var isSearching = false

    /// The entry the transcript should scroll to. W10 observes this; every change is a fresh
    /// token so scrolling to the same entry twice still moves.
    public private(set) var scrollTarget: ScrollTarget?
    /// Bumped when the current match should flash (spec 14 §4).
    public private(set) var flashToken = 0
    /// Set for one beat when stepping wrapped around, so the bar can bounce.
    public private(set) var wrapped: WrapEdge?

    public struct ScrollTarget: Equatable, Sendable {
        public let entryId: String?
        public let token: Int
    }

    public enum WrapEdge: Sendable, Equatable { case start, end }

    private unowned let center: CommandCenter
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var unfilteredMatches: [FindMatch] = []
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var scrollToken = 0
    /// Identity of the current match, kept across a re-search so a streaming update does not
    /// drop the user back to match 1 (spec 14 §6).
    @ObservationIgnored private var anchor: String?

    /// Same debounce as the palette; a find bar that queries per keystroke is the same bug.
    static let debounce = Duration.milliseconds(120)

    init(center: CommandCenter) {
        self.center = center
    }

    // MARK: - Presentation

    public var isPresented: Bool { center.isPresented(.find) }

    /// Opening with a selection seeds the query from it (spec 14 §4).
    public func open(seed: String?) {
        if let seed = seed?.trimmingCharacters(in: .whitespacesAndNewlines), !seed.isEmpty,
           !seed.contains(where: \.isNewline) {
            query = seed
        } else if !query.isEmpty {
            scheduleSearch()
        }
    }

    public func close() {
        searchTask?.cancel()
        searchTask = nil
        query = ""
        matches = []
        unfilteredMatches = []
        currentIndex = 0
        anchor = nil
        scrollTarget = nil
        wrapped = nil
        isSearching = false
    }

    // MARK: - Stepping

    public var hasMatches: Bool { !matches.isEmpty }
    public var current: FindMatch? {
        matches.indices.contains(currentIndex) ? matches[currentIndex] : nil
    }

    /// `n of m`, 1-based and empty-safe.
    public var positionLabel: String {
        guard !matches.isEmpty else { return query.isEmpty ? "" : "0 of 0" }
        return "\(currentIndex + 1) of \(matches.count)"
    }

    public func next() { step(by: 1) }
    public func previous() { step(by: -1) }

    private func step(by delta: Int) {
        guard !matches.isEmpty else { return }
        let raw = currentIndex + delta
        if raw >= matches.count {
            currentIndex = 0
            wrapped = .end
        } else if raw < 0 {
            currentIndex = matches.count - 1
            wrapped = .start
        } else {
            currentIndex = raw
            wrapped = nil
        }
        anchor = current?.id
        announceScroll()
    }

    public func select(_ match: FindMatch) {
        guard let index = matches.firstIndex(of: match) else { return }
        currentIndex = index
        anchor = match.id
        wrapped = nil
        announceScroll()
    }

    public func clearWrap() { wrapped = nil }

    private func announceScroll() {
        scrollToken += 1
        scrollTarget = ScrollTarget(entryId: current?.entryId, token: scrollToken)
        flashToken += 1
    }

    // MARK: - Highlighting

    /// Ranges W10 should paint in one entry: every match in it, plus which one is current.
    public func highlights(forEntry entryId: String) -> (all: [HighlightRange], current: HighlightRange?) {
        let inEntry = matches.filter { $0.entryId == entryId }
        let ranges = inEntry.flatMap(\.ranges)
        let currentRange = current?.entryId == entryId ? current?.ranges.first : nil
        return (ranges, currentRange)
    }

    // MARK: - Searching

    /// Re-runs the query against the transcript as it now stands. The overlay calls this on
    /// every streaming update; the current match is re-anchored by identity, not by index,
    /// so new matches appearing above it do not move the user.
    public func refresh() {
        guard isPresented, !query.isEmpty else { return }
        scheduleSearch(immediate: true)
    }

    private func scheduleSearch(immediate: Bool = false) {
        searchTask?.cancel()
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            unfilteredMatches = []
            matches = []
            currentIndex = 0
            isSearching = false
            return
        }

        generation += 1
        let generation = generation
        isSearching = true
        searchTask = Task { [weak self] in
            if !immediate {
                try? await Task.sleep(for: FindController.debounce)
                if Task.isCancelled { return }
            }
            guard let self else { return }
            let hits = await center.stores.chat.find(trimmed)
            if Task.isCancelled { return }
            // A late reply from a superseded query must never overwrite a newer one.
            guard generation == self.generation else { return }
            self.adopt(hits)
        }
    }

    private func adopt(_ hits: [SearchHit]) {
        isSearching = false
        unfilteredMatches = hits.map(FindMatch.init(hit:))
        applyFilters()
    }

    private func applyFilters() {
        let previousAnchor = anchor ?? current?.id
        matches = unfilteredMatches.filter(passesToggles)
        if let previousAnchor, let index = matches.firstIndex(where: { $0.id == previousAnchor }) {
            currentIndex = index
        } else {
            currentIndex = matches.isEmpty ? 0 : min(currentIndex, matches.count - 1)
        }
        anchor = current?.id
        if !matches.isEmpty { announceScroll() }
    }

    /// Applies the two toggles to a hit by reading the core's ranges out of the core's
    /// snippet. No searching happens here — only a yes/no on what the core already found.
    private func passesToggles(_ match: FindMatch) -> Bool {
        guard caseSensitive || wholeWord else { return true }
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return true }
        let snippet = match.snippet
        for highlight in match.ranges {
            guard let range = highlight.range(in: snippet) else { continue }
            if caseSensitive, String(snippet[range]) != needle { continue }
            if wholeWord, !isWholeWord(range, in: snippet) { continue }
            return true
        }
        return false
    }

    private func isWholeWord(_ range: Range<String.Index>, in text: String) -> Bool {
        let isWordCharacter: (Character) -> Bool = { $0.isLetter || $0.isNumber || $0 == "_" }
        if range.lowerBound > text.startIndex {
            let before = text[text.index(before: range.lowerBound)]
            if isWordCharacter(before) { return false }
        }
        if range.upperBound < text.endIndex {
            let after = text[range.upperBound]
            if isWordCharacter(after) { return false }
        }
        return true
    }
}
