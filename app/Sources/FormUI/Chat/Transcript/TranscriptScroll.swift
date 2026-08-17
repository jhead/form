import SwiftUI
import FormDesign

/// How far from the bottom the user may be and still be considered "at the bottom" — the
/// threshold spec 10 §2 fixes at 40 pt.
let transcriptPinThreshold: CGFloat = 40

/// A scroll view's content height and offset, published up from the content's own geometry.
struct TranscriptMetrics: Equatable {
    var contentHeight: CGFloat = 0
    var offset: CGFloat = 0
    var viewportHeight: CGFloat = 0

    var distanceFromBottom: CGFloat {
        max(0, contentHeight - offset - viewportHeight)
    }

    var isAtBottom: Bool { distanceFromBottom <= transcriptPinThreshold }
    /// Nothing to jump to when the content does not even fill the viewport.
    var overflows: Bool { contentHeight > viewportHeight + transcriptPinThreshold }
}

struct TranscriptMetricsKey: PreferenceKey {
    static let defaultValue = TranscriptMetrics()
    static func reduce(value: inout TranscriptMetrics, nextValue: () -> TranscriptMetrics) {
        let next = nextValue()
        if next != TranscriptMetrics() { value = next }
    }
}

/// Per-session scroll memory, plus the pin decision.
///
/// **Auto-scroll must never fight the user** (spec 10 §2): the transcript follows the tail
/// only while they are within 40 pt of the bottom. Scrolling away sets `isPinned` false and
/// nothing re-pins it but reaching the bottom again or pressing the jump pill.
@MainActor
@Observable
final class TranscriptScrollState {
    private(set) var metrics = TranscriptMetrics()
    private(set) var isPinned = true

    /// Bumped to ask the view to scroll to the tail. `@Observable` needs a value change to
    /// notice, and "scroll now" has no natural value.
    private(set) var scrollRequest = 0

    /// Where each session was left, so a route change restores rather than resets
    /// (spec 10 §2). In-memory: persisting across launches needs a core-side field nobody
    /// owns yet.
    private var remembered: [String: Bool] = [:]
    private var sessionId: String?
    /// False until the first real layout. Before that there is no scroll position to
    /// interpret, and treating the empty one as "scrolled away" shows the pill on load.
    private var hasSettled = false

    /// Upward movement accumulated since the last time the user was heading down. A single
    /// relayout can clamp the offset back by a few points; a scroll gesture keeps going.
    private var upwardDrift: CGFloat = 0

    func update(_ next: TranscriptMetrics) {
        let previous = metrics
        metrics = next

        // Before the first real layout there is no scroll position to interpret; reading the
        // empty one as "scrolled away" is what shows the pill on load.
        guard hasSettled else {
            guard next.contentHeight > 0, next.viewportHeight > 0 else { return }
            hasSettled = true
            if isPinned { scrollRequest += 1 }
            return
        }

        // **Only the user scrolling up breaks the follow.** Content growing under the
        // viewport and our own scroll-to-bottom both move the offset *forward*; a backward
        // move is a wheel or a drag. Keying on "is at the bottom" instead would unpin on
        // every delta, because the tail is briefly below the fold between the layout pass
        // and the scroll that chases it.
        //
        // The movement has to add up to the same 40 pt the pin threshold uses before it
        // counts: re-laying out a long message can clamp the offset back a few points, and
        // treating that as intent is what makes the pill flicker on mid-stream.
        if next.offset < previous.offset {
            upwardDrift += previous.offset - next.offset
            if upwardDrift > transcriptPinThreshold { isPinned = false }
        } else if next.offset > previous.offset {
            upwardDrift = 0
        }
        if next.isAtBottom {
            isPinned = true
            upwardDrift = 0
        }
        if isPinned, next.contentHeight != previous.contentHeight { scrollRequest += 1 }

        if let sessionId { remembered[sessionId] = isPinned }
    }

    /// Follow the tail if the user has not scrolled away.
    func contentGrew() {
        guard isPinned else { return }
        scrollRequest += 1
    }

    func jumpToLatest() {
        isPinned = true
        upwardDrift = 0
        scrollRequest += 1
    }

    func route(to sessionId: String?) {
        guard sessionId != self.sessionId || !hasSettled else { return }
        self.sessionId = sessionId
        isPinned = sessionId.flatMap { remembered[$0] } ?? true
        metrics = TranscriptMetrics()
        upwardDrift = 0
        hasSettled = false
    }
}

/// The "jump to latest" pill (spec 10 §2). Appears only when the user has scrolled away
/// from a transcript that actually overflows.
struct JumpToLatestPill: View {
    @Environment(\.theme) private var theme

    let isVisible: Bool
    let isStreaming: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: theme.metrics.spacing.sm) {
                if isStreaming {
                    PulsingDot(size: theme.metrics.statusDot)
                }
                Text(isStreaming ? "Streaming" : "Jump to latest")
                    .typeStyle(theme.typography.micro)
                Image(systemName: "arrow.down")
                    .typeStyle(theme.typography.micro)
            }
            .foregroundStyle(theme.color.textPrimary)
            .padding(.horizontal, theme.metrics.spacing.lg)
            .frame(height: theme.metrics.chipHeight + theme.metrics.spacing.xs)
            .background(.regularMaterial, in: Capsule(style: .continuous))
            .background(theme.color.surface.opacity(0.6), in: Capsule(style: .continuous))
            .overlay(
                Capsule(style: .continuous)
                    .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
            )
            .shadow(
                color: theme.color.overlay.color.opacity(0.35),
                radius: theme.metrics.spacing.lg, y: theme.metrics.spacing.xs)
            .contentShape(Capsule(style: .continuous))
        }
        .buttonStyle(.plain)
        .opacity(isVisible ? 1 : 0)
        .offset(y: isVisible ? 0 : theme.metrics.spacing.md)
        .allowsHitTesting(isVisible)
        .animation(theme.motion.animation(.normal, curve: .emphasized), value: isVisible)
        .accessibilityLabel("Jump to latest")
    }
}
