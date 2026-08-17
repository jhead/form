import Foundation

/// The Home dashboard's data (F11).
///
/// One `getStats` per `(range, tz)`, cached, refetched when the core says the numbers moved.
/// `stats_invalidated` arrives once per finished run, so refetching eagerly would put a
/// full aggregation on the critical path of every turn — hence the 2 s coalescing floor
/// (spec 07 §4).
@MainActor
@Observable
public final class StatsStore {
    public struct Key: Hashable, Sendable {
        public var range: StatsRange
        public var tz: String
    }

    public var range: StatsRange = .d7
    public private(set) var stats: UsageStats?
    public private(set) var isLoading = false
    public private(set) var lastError: String?

    /// At most one fetch per key per 2 s.
    public static let coalescingInterval: Duration = .seconds(2)

    @ObservationIgnored private let client: CoreClient
    @ObservationIgnored private let timeZone: String
    @ObservationIgnored private var cache: [Key: UsageStats] = [:]
    @ObservationIgnored private var lastFetchedAt: [Key: ContinuousClock.Instant] = [:]
    @ObservationIgnored private var pending: Task<Void, Never>?

    public init(client: CoreClient, timeZone: String = TimeZone.current.identifier) {
        self.client = client
        self.timeZone = timeZone
    }

    public var key: Key { Key(range: range, tz: timeZone) }

    /// Preview seeding — synchronous.
    func seed(_ documents: [StatsRange: UsageStats]) {
        for (range, document) in documents { cache[Key(range: range, tz: timeZone)] = document }
        stats = cache[key]
    }

    /// Switches period, serving the cached document immediately if there is one.
    public func select(_ range: StatsRange) async {
        self.range = range
        if let cached = cache[key] { stats = cached }
        await refresh()
    }

    public func apply(_ event: CoreEvent) {
        guard case .statsInvalidated = event.kind else { return }
        scheduleRefresh()
    }

    /// Fetches now unless the last fetch for this key was under the coalescing floor, in
    /// which case one fetch is scheduled at the boundary and further calls fold into it.
    public func scheduleRefresh() {
        guard pending == nil else { return }
        let key = key
        let elapsed = lastFetchedAt[key].map { ContinuousClock.now - $0 }
        let wait = elapsed.map { Self.coalescingInterval - $0 } ?? .zero

        pending = Task { [weak self] in
            if wait > .zero { try? await Task.sleep(for: wait) }
            guard let self, !Task.isCancelled else { return }
            self.pending = nil
            await self.refresh(force: true)
        }
    }

    public func refresh(force: Bool = false) async {
        let key = key
        if !force, let cached = cache[key] {
            stats = cached
            return
        }
        isLoading = true
        defer { isLoading = false }
        do {
            let result = try await client.query(GetStats(range: key.range, tz: key.tz))
            cache[key] = result
            lastFetchedAt[key] = ContinuousClock.now
            // The period may have changed while the query was in flight.
            if key == self.key { stats = result }
            lastError = nil
        } catch {
            lastError = String(describing: error)
            Log.stores.error("getStats failed: \(String(describing: error), privacy: .public)")
        }
    }

    public func cached(_ key: Key) -> UsageStats? { cache[key] }
}
