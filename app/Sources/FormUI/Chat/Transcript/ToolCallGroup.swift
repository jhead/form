import SwiftUI
import FormCore
import FormDesign

/// Consecutive tool calls, collapsed to one 28 pt row — `Ran 5 commands, used a tool ›` —
/// and expandable to per-call detail (F1.3, spec 08 §1).
struct ToolCallGroup: View {
    @Environment(\.theme) private var theme

    let calls: [ToolCallDisplay]

    @State private var isExpanded = false

    private var summary: ToolGroupSummary { ToolGroupSummary(calls) }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
            header

            if isExpanded {
                VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
                    ForEach(calls) { call in
                        ToolCallRow(call: call)
                    }
                }
                .padding(.leading, theme.metrics.spacing.xl)
                .transition(.opacity)
            }
        }
        .animation(theme.motion.animation(.normal), value: isExpanded)
    }

    private var header: some View {
        Button { isExpanded.toggle() } label: {
            HStack(spacing: theme.metrics.spacing.md) {
                glyph

                Text(summary.phrase)
                    .typeStyle(theme.typography.ui)
                    .foregroundStyle(theme.color.textSecondary)

                if summary.hasDiff {
                    DiffCounts(added: summary.linesAdded, removed: summary.linesRemoved)
                }

                Image(systemName: "chevron.right")
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
                    .rotationEffect(.degrees(isExpanded ? 90 : 0))

                Spacer(minLength: 0)
            }
            .frame(height: theme.metrics.toolRowHeight)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(summary.phrase)
        .accessibilityHint(isExpanded ? "Collapse tool calls" : "Expand tool calls")
    }

    @ViewBuilder
    private var glyph: some View {
        if summary.isRunning {
            PulsingDot(size: theme.metrics.statusDot)
        } else if summary.hasError {
            Image(systemName: "exclamationmark.triangle.fill")
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.danger)
        } else {
            Image(systemName: "wrench.and.screwdriver")
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.textTertiary)
        }
    }
}

/// `+268` / `-0`, tabular (spec 08 §1). The tokens are `diffAdd` / `diffRemove`; the wire
/// fields the counts come from are `linesAdded` / `linesRemoved`.
struct DiffCounts: View {
    @Environment(\.theme) private var theme

    let added: Int64
    let removed: Int64

    var body: some View {
        let text = ChatFormat.diff(added: added, removed: removed)
        HStack(spacing: theme.metrics.spacing.xs) {
            Text(text.added)
                .foregroundStyle(theme.color.diffAdd)
            Text(text.removed)
                .foregroundStyle(theme.color.diffRemove)
        }
        .typeStyle(theme.typography.micro)
        .tabularFigures()
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(added) added, \(removed) removed")
    }
}

/// One call in the expanded group: name, argument summary, duration, status, and a
/// disclosure for the full arguments and result (spec 10 §4).
private struct ToolCallRow: View {
    @Environment(\.theme) private var theme

    let call: ToolCallDisplay

    @State private var isExpanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
            Button { isExpanded.toggle() } label: {
                HStack(spacing: theme.metrics.spacing.md) {
                    status

                    Text(call.name)
                        .typeStyle(theme.typography.micro.weighted(.medium))
                        .foregroundStyle(theme.color.textPrimary)

                    if let argument = call.argumentSummary {
                        Text(argument)
                            .typeStyle(theme.typography.micro)
                            .foregroundStyle(theme.color.textTertiary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }

                    Spacer(minLength: theme.metrics.spacing.md)

                    if let duration = call.durationMs {
                        Text(ChatFormat.duration(duration))
                            .typeStyle(theme.typography.micro)
                            .tabularFigures()
                            .foregroundStyle(theme.color.textTertiary)
                    }

                    if let added = call.linesAdded {
                        DiffCounts(added: added, removed: call.linesRemoved ?? 0)
                    }
                }
                .frame(height: theme.metrics.toolRowHeight)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            // Indeterminate until the tool reports progress, determinate after (F6.2).
            if call.isRunning {
                if let progress = call.progress {
                    ProgressBar(value: progress)
                } else {
                    ProgressBar(value: nil)
                        .shimmer()
                }
            }

            if isExpanded {
                detail.transition(.opacity)
            }
        }
        .animation(theme.motion.animation(.normal), value: isExpanded)
    }

    @ViewBuilder
    private var status: some View {
        if call.isRunning {
            PulsingDot(size: theme.metrics.statusDot)
        } else if call.isError {
            Image(systemName: "xmark.circle.fill")
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.danger)
        } else {
            Image(systemName: "checkmark.circle")
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.success)
        }
    }

    @ViewBuilder
    private var detail: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.md) {
            if let json = call.argumentsJSON {
                DetailBlock(title: "Arguments", body: json)
            }
            // A result that is a path is a file, not prose — spec 10 §4.
            if let path = call.resultPath, call.resultText == nil {
                Chip(URL(fileURLWithPath: path).lastPathComponent, systemImage: "doc", tooltip: path)
            } else if let text = call.resultText {
                DetailBlock(title: call.isError ? "Error" : "Result", body: text)
            } else if let json = call.resultJSON {
                DetailBlock(title: "Result", body: json)
            }
        }
        .padding(.leading, theme.metrics.spacing.xl)
        .padding(.bottom, theme.metrics.spacing.md)
    }
}

private struct DetailBlock: View {
    @Environment(\.theme) private var theme

    let title: String
    let body_: String

    init(title: String, body: String) {
        self.title = title
        body_ = body
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
            Text(title)
                .typeStyle(theme.typography.micro.weighted(.medium))
                .foregroundStyle(theme.color.textTertiary)

            ScrollView(.horizontal, showsIndicators: false) {
                Text(body_)
                    .typeStyle(theme.typography.code)
                    .foregroundStyle(theme.color.textSecondary)
                    .textSelection(.enabled)
                    .padding(theme.metrics.spacing.md)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                    .fill(theme.color.surfaceRaised)
            )
        }
    }
}
