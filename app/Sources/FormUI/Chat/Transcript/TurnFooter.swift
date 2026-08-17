import SwiftUI
import FormCore
import FormDesign

/// `3m 31s · 5.9k tokens` at 11 pt tertiary, preceded by a small glyph (F1.4, spec 08 §1).
struct TurnFooter: View {
    @Environment(\.theme) private var theme

    let model: TurnFooterModel

    var body: some View {
        HStack(spacing: theme.metrics.spacing.sm) {
            Image(systemName: glyph)
                .typeStyle(theme.typography.micro)
                .foregroundStyle(tint)

            Text(text)
                .typeStyle(theme.typography.micro)
                .tabularFigures()
                .foregroundStyle(tint)

            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
    }

    private var text: String {
        var parts: [String] = []
        if let duration = model.durationMs { parts.append(ChatFormat.duration(duration)) }
        if model.totalTokens > 0 {
            parts.append("\(ChatFormat.compact(model.totalTokens)) tokens")
        }
        if let note = model.note { parts.append(note) }
        return parts.joined(separator: " · ")
    }

    private var glyph: String {
        switch model.stopReason {
        case .aborted: "stop.circle"
        case .error: "exclamationmark.triangle"
        case .length: "scissors"
        default: "checkmark.circle"
        }
    }

    private var tint: ThemeColor {
        switch model.stopReason {
        case .aborted, .length: theme.color.textSecondary
        case .error: theme.color.danger
        default: theme.color.textTertiary
        }
    }
}

/// A failed run, inline with a retry — not a toast (spec 10 §3).
struct InlineErrorBlock: View {
    @Environment(\.theme) private var theme

    let model: InlineErrorModel
    let onRetry: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: theme.metrics.spacing.lg) {
            Image(systemName: "exclamationmark.triangle.fill")
                .typeStyle(theme.typography.caption)
                .foregroundStyle(theme.color.danger)

            VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
                if let code = model.code {
                    Text(code)
                        .typeStyle(theme.typography.micro.weighted(.medium))
                        .foregroundStyle(theme.color.danger)
                }
                Text(model.message)
                    .typeStyle(theme.typography.caption)
                    .foregroundStyle(theme.color.textPrimary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Spacer(minLength: theme.metrics.spacing.md)

            FormButton("Retry", systemImage: "arrow.clockwise", size: .small, action: onRetry)
        }
        .padding(theme.metrics.spacing.lg)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .fill(theme.color.danger.opacity(0.10))
        )
        .overlay(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .strokeBorder(theme.color.danger.opacity(0.35), lineWidth: theme.metrics.hairline * 2)
        )
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Run failed: \(model.message)")
    }
}

/// A prompt typed while a run was in flight (F1.7). Right-aligned like the bubble it will
/// become, dimmed, with a cancel affordance.
struct QueuedMessageRow: View {
    @Environment(\.theme) private var theme

    let text: String
    let columnWidth: CGFloat
    let onCancel: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: theme.metrics.spacing.md) {
            Spacer(minLength: theme.metrics.spacing.xl)

            IconButton(
                systemImage: "xmark", accessibilityLabel: "Cancel queued message",
                size: .small, action: onCancel)

            HStack(spacing: theme.metrics.spacing.md) {
                Image(systemName: "clock")
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)

                Text(text)
                    .typeStyle(theme.typography.body)
                    .foregroundStyle(theme.color.textSecondary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.vertical, theme.metrics.spacing.lg)
            .padding(.horizontal, 14)
            .background(
                RoundedRectangle(cornerRadius: theme.metrics.radius.xl, style: .continuous)
                    .fill(theme.color.surfaceRaised.opacity(0.6))
            )
            .overlay(
                RoundedRectangle(cornerRadius: theme.metrics.radius.xl, style: .continuous)
                    .strokeBorder(
                        theme.color.border,
                        style: StrokeStyle(
                            lineWidth: theme.metrics.hairline * 2,
                            dash: [theme.metrics.spacing.xs, theme.metrics.spacing.xs]))
            )
            .frame(
                maxWidth: max(0, columnWidth) * theme.metrics.messageMaxWidthFraction,
                alignment: .trailing)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Queued: \(text)")
    }
}
