import SwiftUI
import FormCore
import FormDesign

/// The composer's 14 pt context ring and its breakdown popover (F10, spec 10 §7).
///
/// Every number here is the core's — `ContextUsage` is computed in Rust from the real
/// transcript (F10.4). The view animates between values and recolors at 75 % / 90 %, both of
/// which `ProgressRing` already does.
struct ContextRingButton: View {
    @Environment(\.theme) private var theme

    let usage: ContextUsage?

    @State private var isShowingPopover = false

    var body: some View {
        Button { isShowingPopover.toggle() } label: {
            ProgressRing(value: usage?.fraction ?? 0)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .formTooltip(tooltip)
        .accessibilityLabel("Context usage")
        .popover(isPresented: $isShowingPopover, arrowEdge: .top) {
            PopoverContainer(title: "Context") {
                if let usage {
                    breakdown(usage)
                } else {
                    Text("No usage reported yet.")
                        .typeStyle(theme.typography.caption)
                        .foregroundStyle(theme.color.textTertiary)
                }
            }
        }
    }

    private var tooltip: String? {
        guard let usage else { return nil }
        return "\(ChatFormat.exact(usage.used)) / \(ChatFormat.exact(usage.total)) tokens"
    }

    @ViewBuilder
    private func breakdown(_ usage: ContextUsage) -> some View {
        PopoverRow(
            "Used",
            value: "\(ChatFormat.compact(usage.used)) / \(ChatFormat.compact(usage.total))",
            fraction: usage.fraction,
            tint: tint(for: usage.fraction))

        FormDivider()

        // The core's own segment order (spec 04); the view does not sort it.
        ForEach(usage.segments) { segment in
            PopoverRow(
                segment.kind.displayName,
                value: ChatFormat.exact(segment.tokens),
                fraction: usage.total == 0 ? 0 : Double(segment.tokens) / Double(usage.total),
                tint: color(for: segment.kind))
        }

        FormDivider()

        PopoverRow("Messages", value: ChatFormat.exact(usage.messageCount))
        PopoverRow("Session cost", value: ChatFormat.cost(usage.cost.total))
    }

    private func tint(for fraction: Double) -> ThemeColor {
        if fraction >= 0.90 { return theme.color.danger }
        if fraction >= 0.75 { return theme.color.warning }
        return theme.color.accent
    }

    /// Segments keep a stable color so the bars read as one chart across sessions.
    private func color(for kind: SegmentKind) -> ThemeColor {
        switch kind {
        case .system: theme.color.info
        case .tools: theme.color.series(1)
        case .transcript: theme.color.accent
        case .attachments: theme.color.series(3)
        case .outputReserve: theme.color.textTertiary
        default: theme.color.textSecondary
        }
    }
}
