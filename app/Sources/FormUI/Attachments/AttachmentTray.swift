import FormCore
import FormDesign
import SwiftUI

/// The pre-send tray (F3.5, F3.6): a horizontal row of 56 pt chips above the composer input.
///
/// Rejections live here rather than in a toast. W8 drew that line deliberately — a rejection
/// is about a specific chip the user is looking at, and a toast would float away from it.
public struct AttachmentTray: View {
    @Environment(\.theme) private var theme

    private let intake: AttachmentIntake

    public init(intake: AttachmentIntake) {
        self.intake = intake
    }

    public var body: some View {
        if intake.hasItems {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.sm) {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: theme.metrics.spacing.sm) {
                        ForEach(intake.items) { item in
                            AttachmentTrayChip(
                                item: item,
                                store: intake.thumbnails,
                                onRemove: { intake.remove(item) }
                            )
                        }
                    }
                    .padding(.vertical, theme.metrics.spacing.xxs)
                }
                .frame(height: theme.metrics.attachmentChipHeight + theme.metrics.spacing.xs)

                if !rejections.isEmpty {
                    rejectionSummary
                }
            }
            .transition(.opacity)
            .animation(theme.motion.animation(.fast), value: intake.items)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Attachments")
        }
    }

    private var rejections: [PendingAttachment] {
        intake.items.filter { $0.rejection != nil }
    }

    private var rejectionSummary: some View {
        HStack(alignment: .top, spacing: theme.metrics.spacing.md) {
            Image(systemName: "exclamationmark.triangle")
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.danger)
            VStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
                ForEach(rejections) { item in
                    Text("\(item.filename) — \(item.rejection ?? "")")
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
            IconButton(
                systemImage: "xmark", accessibilityLabel: "Dismiss", size: .small,
                action: intake.dismissRejections)
        }
        .padding(.horizontal, theme.metrics.spacing.md)
        .padding(.vertical, theme.metrics.spacing.sm)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                .fill(theme.color.danger.opacity(0.12))
        )
    }
}

/// One 56 pt chip: 40 pt thumbnail, filename truncating in the middle, size, and a remove `×`
/// that appears on hover.
struct AttachmentTrayChip: View {
    @Environment(\.theme) private var theme

    let item: PendingAttachment
    let store: ThumbnailStore
    let onRemove: () -> Void

    @State private var isHovering = false

    private var isRejected: Bool { item.rejection != nil }

    var body: some View {
        HStack(spacing: theme.metrics.spacing.md) {
            AttachmentThumbnailView(
                store: store, sha256: item.sha256, source: item.source, mime: item.mime)
            .opacity(isRejected ? 0.4 : 1)

            VStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
                Text(AttachmentFormat.middleTruncated(item.filename))
                    .typeStyle(theme.typography.caption)
                    .foregroundStyle(theme.color.textPrimary)
                    .lineLimit(1)
                HStack(spacing: theme.metrics.spacing.xs) {
                    Text(item.sizeText)
                        .typeStyle(theme.typography.micro)
                        .tabularFigures()
                        .foregroundStyle(theme.color.textTertiary)
                    if item.state == .adding {
                        Text("adding…")
                            .typeStyle(theme.typography.micro)
                            .foregroundStyle(theme.color.textTertiary)
                    } else if isRejected {
                        Badge("rejected", tone: .danger)
                    }
                }
            }

            IconButton(
                systemImage: "xmark", accessibilityLabel: "Remove \(item.filename)", size: .small,
                action: onRemove
            )
            .opacity(isHovering ? 1 : 0)
        }
        .padding(.horizontal, theme.metrics.spacing.md)
        .frame(height: theme.metrics.attachmentChipHeight)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .fill(theme.color.surfaceRaised)
        )
        .overlay(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .strokeBorder(
                    isRejected ? theme.color.danger : theme.color.border,
                    lineWidth: theme.metrics.hairline * 2)
        )
        .onHover { isHovering = $0 }
        .animation(theme.motion.animation(.fast), value: isHovering)
        .formTooltip(item.filename, detail: item.rejection)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(item.filename), \(item.sizeText)\(isRejected ? ", rejected" : "")")
    }
}

#Preview("Attachment tray") {
    AttachmentTrayPreview()
}

private struct AttachmentTrayPreview: View {
    @State private var intake: AttachmentIntake = {
        let intake = AttachmentIntake(stores: CoreStores.preview(.populated))
        intake.seed(PendingAttachment.previewItems)
        return intake
    }()

    var body: some View {
        ThemePreview {
            AttachmentTray(intake: intake)
        }
        .frame(width: 680)
    }
}

extension PendingAttachment {
    /// Preview fixtures. The tray's three states in one row.
    static var previewItems: [PendingAttachment] {
        [
            PendingAttachment(
                id: UUID(), source: .file(URL(fileURLWithPath: "/tmp/sidebar-reference.png")),
                filename: "sidebar-reference.png", mime: "image/png", bytes: 284_910,
                sha256: String(repeating: "a1", count: 32), state: .ready(attachmentId: "att_1")),
            PendingAttachment(
                id: UUID(), source: .file(URL(fileURLWithPath: "/tmp/spec.pdf")),
                filename: "spec.pdf", mime: "application/pdf", bytes: 1_284_910,
                sha256: String(repeating: "b2", count: 32), state: .adding),
            PendingAttachment(
                id: UUID(), source: .file(URL(fileURLWithPath: "/tmp/capture.mov")),
                filename: "screen-capture-2026-08-16.mov", mime: "video/quicktime",
                bytes: 12_582_912, sha256: String(repeating: "c3", count: 32),
                state: .rejected(reason: "Unsupported type: video/quicktime")),
        ]
    }
}
