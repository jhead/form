import AppKit
import FormDesign
import SwiftUI
import UniformTypeIdentifiers

/// Drag-and-drop and paste, as one modifier (F3.1).
///
/// The highlight covers the *whole composer*, not just the field, because that is the target
/// the user is aiming at — spec 13 asks for a full-composer drop highlight.
///
/// W10 applies this to the composer and W9 may apply it to the transcript; both routes land in
/// the same `AttachmentIntake`, so there is one policy and one tray.
public struct AttachmentDropTarget: ViewModifier {
    @Environment(\.theme) private var theme

    private let intake: AttachmentIntake
    private let acceptsPaste: Bool

    @State private var isTargeted = false

    public init(intake: AttachmentIntake, acceptsPaste: Bool = true) {
        self.intake = intake
        self.acceptsPaste = acceptsPaste
    }

    public func body(content: Content) -> some View {
        content
            .overlay {
                if isTargeted {
                    RoundedRectangle(cornerRadius: theme.metrics.radius.xl, style: .continuous)
                        .fill(theme.color.accentMuted.opacity(0.18))
                        .overlay(
                            RoundedRectangle(
                                cornerRadius: theme.metrics.radius.xl, style: .continuous
                            )
                            .strokeBorder(
                                theme.color.borderFocus,
                                style: StrokeStyle(
                                    lineWidth: theme.metrics.hairline * 4, dash: [6, 4]))
                        )
                        .overlay {
                            Label("Drop to attach", systemImage: "paperclip")
                                .typeStyle(theme.typography.uiMedium)
                                .foregroundStyle(theme.color.textSecondary)
                        }
                        .allowsHitTesting(false)
                        .transition(.opacity)
                }
            }
            .onDrop(of: [.fileURL, .image], isTargeted: $isTargeted) { providers in
                intake.add(drop: providers)
            }
            .animation(theme.motion.animation(.fast), value: isTargeted)
            .modifier(PasteCommand(intake: intake, isEnabled: acceptsPaste))
    }
}

/// `⌘V` of file URLs or image data. Registered as a command rather than a key handler so it
/// only fires when this view is in the responder chain, and so W14's table stays the only
/// place a *shortcut* is declared — this is the standard paste command, not a new binding.
private struct PasteCommand: ViewModifier {
    let intake: AttachmentIntake
    let isEnabled: Bool

    func body(content: Content) -> some View {
        if isEnabled {
            content.onPasteCommand(of: [.fileURL, .image, .png, .tiff]) { _ in
                // The providers SwiftUI hands over cannot carry the pasteboard's file
                // promises reliably; reading the pasteboard directly is both simpler and the
                // only way to get a pasted screenshot's PNG.
                _ = intake.paste()
            }
        } else {
            content
        }
    }
}

extension View {
    /// The composer's drop-and-paste surface (F3.1).
    public func attachmentDropTarget(_ intake: AttachmentIntake, acceptsPaste: Bool = true)
        -> some View {
        modifier(AttachmentDropTarget(intake: intake, acceptsPaste: acceptsPaste))
    }
}
