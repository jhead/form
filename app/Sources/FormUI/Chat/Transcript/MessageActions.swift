import AppKit
import SwiftUI
import FormCore
import FormDesign

/// Copy / retry / branch plus a relative timestamp, revealed on hover with a `motion.fast`
/// fade (F1.5). Laid out in the gutter beside the message rather than over it, so nothing
/// the user is reading moves when the row lights up.
struct MessageActions: View {
    @Environment(\.theme) private var theme

    let timestamp: TimestampMs
    let isVisible: Bool
    var showsRetry = true
    var showsBranch = true
    let copyText: String
    let onRetry: () -> Void
    let onBranch: () -> Void

    @State private var didCopy = false

    var body: some View {
        HStack(spacing: theme.metrics.spacing.xxs) {
            Text(ChatFormat.relative(timestamp))
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.textTertiary)
                .padding(.trailing, theme.metrics.spacing.xxs)

            IconButton(
                systemImage: didCopy ? "checkmark" : "doc.on.doc",
                accessibilityLabel: didCopy ? "Copied" : "Copy message",
                size: .small,
                action: copy)

            if showsRetry {
                IconButton(
                    systemImage: "arrow.clockwise", accessibilityLabel: "Retry from here",
                    size: .small, action: onRetry)
            }
            if showsBranch {
                IconButton(
                    systemImage: "arrow.triangle.branch", accessibilityLabel: "Branch from here",
                    size: .small, action: onBranch)
            }
        }
        .opacity(isVisible ? 1 : 0)
        .allowsHitTesting(isVisible)
        .animation(theme.motion.animation(.fast), value: isVisible)
        .animation(theme.motion.animation(.fast), value: didCopy)
    }

    private func copy() {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(copyText, forType: .string)
        didCopy = true
        // The checkmark dwell is `motion.pulse` — the one token long enough to read as a
        // confirmation rather than a flicker.
        let dwell = theme.motion.pulse
        Task {
            try? await Task.sleep(for: .seconds(dwell))
            didCopy = false
        }
    }
}
