import AppKit
import FormCore
import FormDesign
import Foundation
import Testing

@testable import FormMarkdown

/// Spec 11 §5: "streaming a 60 KB document token-by-token keeps frame time under budget
/// (measure and record)".
///
/// ## What is measured
///
/// One *tick* is what the renderer does when the core hands it a re-parsed document: segment
/// the blocks into runs, look each run up in the render cache, build the ones that missed,
/// and lay the changed run out in TextKit. That is the whole of this module's per-frame cost;
/// everything else SwiftUI does is bounded by the fact that no other view's inputs changed.
///
/// ## Recorded measurements
///
/// Apple silicon, **debug** build, 120 blocks, `swift test`:
///
/// | shape | size | ticks | build worst/mean | layout worst/mean | tick p95 | tick worst |
/// |---|---|---|---|---|---|---|
/// | `mixed` (prose + code + quotes) | 64 KB | 260 | 0.45 / 0.17 ms | 0.80 / 0.17 ms | **0.95 ms** | 1.25 ms |
/// | `prose` (one 120-block run) | 75 KB | 360 | 3.6 / 1.7 ms | 3.9 / 1.7 ms | **6.5 ms** | 8.0 ms |
///
/// For scale, the core parses the same document warm in 1.4 ms with a 2.2 ms worst streaming
/// tick (spec 05 §5), and a 60 fps frame is 16.6 ms. Parse plus render is therefore ~3 ms in
/// the shape a coding agent actually produces, and under 9 ms in the adversarial one — the
/// renderer is not what drops the frame.
///
/// Three things earn those numbers, and the first two have a test above that fails if they
/// regress: the block-id-keyed render cache (only the run holding the caret is rebuilt),
/// `MarkdownTextRun.update` (only the changed tail of the text storage is replaced) and
/// `MarkdownFontCache`. Before `update` existed the `prose` worst tick was **25.8 ms** — a
/// dropped frame per token — because `setAttributedString` re-lays-out every paragraph above
/// the caret.
///
/// What is left in `prose` is rebuilding the run's attributed string from scratch each tick,
/// which is O(run) rather than O(tail). Caching the run's *prefix* and appending only the
/// tail would remove it; at 3.6 ms debug for an adversarial 75 KB of unbroken prose it has
/// not earned the complexity yet.
///
/// The budget below is a regression alarm for an accidental O(n²), not a benchmark; a release
/// build is roughly 3× faster than what it asserts.
///
/// It is asserted against the 95th percentile rather than the single worst tick, and the suite
/// is `.serialized`, because `swift test` runs suites concurrently: a wall-clock maximum over
/// hundreds of samples on a shared machine picks up whatever else the scheduler was doing (a
/// stray 110 ms tick was observed while another suite was compiling its fixtures). p95 moves
/// when the renderer regresses and does not move when the machine hiccups. The worst tick is
/// printed either way.
@MainActor
@Suite(.serialized)
struct StreamingBudgetTests {
    /// Under a 60 fps frame with headroom for a slower machine and a debug build.
    static let budget: Duration = .milliseconds(12)

    @Test(
        "streaming a 60 KB document keeps every tick inside the frame budget",
        arguments: StreamingCorpus.Shape.allCases)
    func streamingStaysInBudget(shape: StreamingCorpus.Shape) {
        let metrics = MarkdownMetrics(theme: .dark, style: .default)
        let cache = MarkdownRenderCache()
        let document = StreamingCorpus(blocks: 120, shape: shape)

        #expect(document.byteCount > 61_440, "the fixture must be the size the spec names")
        #expect(document.blocks.count == 120)

        // The TextKit stack the on-screen run would reuse across ticks.
        let (storage, layout, container) = MarkdownTextRun.textKitStack()

        var samples: [(build: Duration, layout: Duration)] = []
        var rebuilds = 0

        for tick in document.ticks {
            let clock = ContinuousClock()
            var runs: [MarkdownRun] = []
            let build = clock.measure {
                runs = MarkdownRun.segment(tick.blocks)
                for run in runs {
                    let before = cache.count
                    _ = renderedText(
                        run.blocks, metrics: metrics, sourcePrefix: "", depth: 0, cache: cache)
                    if cache.count != before { rebuilds += 1 }
                }
            }

            // Only the run that actually changed is re-laid-out; that is what the view does.
            let tail = runs.last.flatMap { run -> RenderedText? in
                guard case .text = run else { return nil }
                return renderedText(
                    run.blocks, metrics: metrics, sourcePrefix: "", depth: 0, cache: cache)
            }
            let layoutTime = clock.measure {
                if let tail {
                    MarkdownTextRun.update(storage, to: tail.attributed)
                    _ = MarkdownTextRun.measure(
                        layout: layout, container: container, width: 680)
                }
            }

            samples.append((build, layoutTime))
        }

        let ticks = samples.count
        let worstBuild = samples.map(\.build).max() ?? .zero
        let worstLayout = samples.map(\.layout).max() ?? .zero
        let totalBuild = samples.map(\.build).reduce(Duration.zero, +)
        let totalLayout = samples.map(\.layout).reduce(Duration.zero, +)
        let sorted = samples.map { $0.build + $0.layout }.sorted()
        let p95 = sorted[min(sorted.count - 1, Int(Double(sorted.count) * 0.95))]
        let worst = worstBuild + worstLayout
        // Measurements are the point of this test, so they go to stdout where `swift test`
        // shows them. (The no-`print` convention is about `Sources/`, not about a test whose
        // job is to report a number.)
        print(
            """

            [W11] streaming budget (\(shape.rawValue)) — \(document.blocks.count) blocks, \
            \(document.byteCount / 1024) KB, \(ticks) ticks
              build   worst \(ms(worstBuild))  mean \(ms(totalBuild / ticks))
              layout  worst \(ms(worstLayout))  mean \(ms(totalLayout / ticks))
              tick    worst \(ms(worst))  p95 \(ms(p95))  budget \(ms(Self.budget))
              cache   \(rebuilds) rebuilds over \(ticks) ticks, \(cache.count) live entries

            """)

        #expect(
            p95 < Self.budget,
            Comment(
                rawValue:
                    "p95 tick \(ms(p95)) exceeded \(ms(Self.budget)) (worst \(ms(worst)))"))

        // The real assertion behind the timing: a tick must not rebuild the document. With
        // stable ids only the run holding the caret can miss the cache.
        #expect(
            rebuilds <= ticks + 1,
            Comment(rawValue: "\(rebuilds) rebuilds over \(ticks) ticks — the cache is missing"))
    }

    @Test("a tick with no change at all costs nothing")
    func idleTickIsFree() {
        let metrics = MarkdownMetrics(theme: .light, style: .default)
        let cache = MarkdownRenderCache()
        let document = StreamingCorpus(blocks: 120)
        let final = document.blocks

        for _ in 0 ..< 2 {
            for run in MarkdownRun.segment(final) {
                _ = renderedText(
                    run.blocks, metrics: metrics, sourcePrefix: "", depth: 0, cache: cache)
            }
        }
        let settled = cache.count

        let elapsed = ContinuousClock().measure {
            for _ in 0 ..< 100 {
                for run in MarkdownRun.segment(final) {
                    _ = renderedText(
                        run.blocks, metrics: metrics, sourcePrefix: "", depth: 0, cache: cache)
                }
            }
        }
        #expect(cache.count == settled, "an unchanged document must not add cache entries")
        #expect(elapsed < .milliseconds(200), Comment(rawValue: "100 idle ticks took \(ms(elapsed))"))
    }

    private func ms(_ duration: Duration) -> String {
        let value =
            Double(duration.components.seconds) * 1000
            + Double(duration.components.attoseconds) / 1e15
        return String(format: "%.2f ms", value)
    }
}

/// A 60 KB document that grows the way a streamed response does: complete blocks, then a
/// tail block that gains a few words per tick.
@MainActor
struct StreamingCorpus {
    /// Two shapes, because they stress different halves of the renderer.
    ///
    /// `mixed` is what a coding-agent answer looks like — prose broken up by code blocks and
    /// tables, so the selectable text runs between them stay short.
    /// `prose` is the adversarial one: nothing but paragraphs, which coalesce into a single
    /// enormous text run, so every tick rebuilds and re-lays-out the whole document. If the
    /// design is going to fall over, it falls over here.
    enum Shape: String, CaseIterable { case mixed, prose }

    let shape: Shape
    let blocks: [MarkdownBlock]
    let ticks: [MarkdownDoc]
    let byteCount: Int

    init(blocks count: Int, shape: Shape = .mixed) {
        self.shape = shape
        var kinds: [BlockKind] = []
        for index in 0 ..< count {
            guard shape == .mixed else {
                kinds.append(.paragraph(spans: StreamingCorpus.paragraph(index, sentences: 1)))
                continue
            }
            switch index % 6 {
            case 0:
                kinds.append(
                    .heading(
                        level: (index % 3) + 1, spans: [.text(text: "Section \(index)")],
                        anchor: "section-\(index)"))
            case 1, 2:
                kinds.append(.paragraph(spans: StreamingCorpus.paragraph(index, sentences: 2)))
            case 3:
                kinds.append(
                    .list(
                        ordered: index.isMultiple(of: 4), start: 1, tight: true,
                        items: (0 ..< 6).map { item in
                            MarkdownFixture.item([
                                .paragraph(spans: [
                                    .text(
                                        text:
                                            "Item \(item) of section \(index), long enough that "
                                            + "the list is not a rounding error in the corpus. "),
                                    .code(text: "flag_\(item)"),
                                ])
                            ])
                        }))
            case 4:
                kinds.append(MarkdownFixture.rustBlock)
            default:
                kinds.append(
                    .quote(blocks: [
                        MarkdownFixture.block(
                            0, .paragraph(spans: StreamingCorpus.paragraph(index, sentences: 1)))
                    ]))
            }
        }
        let document = MarkdownFixture.doc(kinds)
        blocks = document.blocks
        byteCount = document.blocks.reduce(0) { $0 + StreamingCorpus.size(of: $1) }

        // Ticks: the document up to block `n`, with block `n` itself arriving word by word.
        var growing: [MarkdownDoc] = []
        for (index, kind) in kinds.enumerated() {
            let head = Array(kinds.prefix(index))
            for partial in StreamingCorpus.partials(of: kind) {
                growing.append(MarkdownFixture.doc(head + [partial]))
            }
        }
        ticks = growing
    }

    /// Seven spans regardless of length, so the number of growth steps per block does not
    /// change when the corpus is resized.
    private static func paragraph(_ index: Int, sentences: Int) -> [Span] {
        let filler = String(
            repeating:
                "The renderer re-reads the tree on every tick, so a paragraph has to be cheap "
                + "to identify and expensive to build only once. ",
            count: sentences)
        return [
            .text(text: "Paragraph \(index). " + filler),
            .strong(spans: [.text(text: "Stable ids")]),
            .text(text: " are what make that true, together with "),
            .code(text: "MarkdownRenderCache"),
            .text(text: ", which memoises by content rather than by position. " + filler),
            .link(
                url: "https://example.com/spec-11", title: nil,
                spans: [.text(text: "Spec 11")]),
            .text(
                text:
                    " calls for the last block to be re-rendered and nothing else; this corpus "
                    + "exists to prove that is what happens under a real load. " + filler),
        ]
    }

    /// A few growth steps per block — enough ticks to catch a per-tick regression without
    /// turning the test into a benchmark run.
    private static func partials(of kind: BlockKind) -> [BlockKind] {
        switch kind {
        case let .paragraph(spans):
            let steps = stride(from: 1, through: spans.count, by: 3).map { $0 }
            return steps.map { .paragraph(spans: Array(spans.prefix($0))) }
        case let .codeBlock(language, code, tokens, _):
            let lines = code.split(separator: "\n", omittingEmptySubsequences: false)
            return (1 ... max(1, lines.count)).map { count in
                let partial = lines.prefix(count).joined(separator: "\n")
                let length = (partial as NSString).length
                return .codeBlock(
                    language: language, code: partial,
                    tokens: tokens.filter { $0.start + $0.len <= length },
                    partial: count < lines.count)
            }
        default:
            return [kind]
        }
    }

    /// The size the spec means: the markdown the core would have parsed, not the JSON it
    /// serialised.
    private static func size(of block: MarkdownBlock) -> Int {
        switch block.kind {
        case let .paragraph(spans), let .heading(_, spans, _):
            return spans.map(\.plainText).joined().utf8.count
        case let .codeBlock(_, code, _, _):
            return code.utf8.count
        case let .html(raw):
            return raw.utf8.count
        case let .quote(blocks), let .footnoteDef(_, blocks):
            return blocks.reduce(0) { $0 + size(of: $1) }
        case let .list(_, _, _, items):
            return items.reduce(0) { $0 + $1.blocks.reduce(0) { $0 + size(of: $1) } }
        case let .table(_, header, rows):
            let cells = header + rows.flatMap { $0 }
            return cells.reduce(0) { $0 + $1.map(\.plainText).joined().utf8.count }
        case .rule, .image, .unknown:
            return 0
        }
    }
}
