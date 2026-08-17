import FormCore
import Foundation

/// Memoizes the expensive half of rendering — building the attributed string and the source
/// map for a text run — keyed by content rather than by position.
///
/// This is what makes F7.3 affordable. A block id from the core is a hash of the block
/// (spec 05 §2), so a run's key changes **exactly** when its rendering would change: during
/// streaming, every run above the caret hits the cache and only the tail is rebuilt.
///
/// Capacity mirrors the core's own memo (spec 05 §5). Eviction is true LRU because a
/// streaming tail otherwise churns 1000+ single-use entries through the cache and pushes out
/// the stable runs that are re-read on every frame.
@MainActor
final class MarkdownRenderCache {
    static let shared = MarkdownRenderCache()

    private struct Entry {
        let value: RenderedText
        var lastUsed: UInt64
    }

    private var entries: [String: Entry] = [:]
    private var clock: UInt64 = 0
    private let capacity: Int

    init(capacity: Int = 512) {
        self.capacity = capacity
    }

    func rendered(key: String, build: () -> RenderedText) -> RenderedText {
        clock &+= 1
        if var hit = entries[key] {
            hit.lastUsed = clock
            entries[key] = hit
            return hit.value
        }
        let value = build()
        entries[key] = Entry(value: value, lastUsed: clock)
        evictIfNeeded()
        return value
    }

    /// Only ever runs on a miss, so the linear scan costs one pass per new block — not per
    /// lookup and not per frame.
    private func evictIfNeeded() {
        guard entries.count > capacity else { return }
        let excess = entries.count - capacity
        let oldest = entries.sorted { $0.value.lastUsed < $1.value.lastUsed }.prefix(excess)
        for (key, _) in oldest { entries.removeValue(forKey: key) }
    }

    var count: Int { entries.count }

    func removeAll() {
        entries.removeAll()
        clock = 0
    }
}

@MainActor
func renderedText(
    _ blocks: [MarkdownBlock], metrics: MarkdownMetrics, sourcePrefix: String, depth: Int,
    cache: MarkdownRenderCache = .shared
) -> RenderedText {
    let key = "\(metrics.cacheKey)|\(sourcePrefix)|\(depth)|"
        + blocks.map(\.id).joined(separator: ",")
    return cache.rendered(key: key) {
        MarkdownAttributedBuilder.render(blocks, metrics: metrics, sourcePrefix: sourcePrefix)
    }
}
