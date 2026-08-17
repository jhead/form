import Observation
import SwiftUI

public struct ToastMessage: Identifiable, Sendable, Equatable {
    public let id: UUID
    public var tone: FormTone
    public var title: String
    public var message: String?
    /// Seconds before auto-dismissal. `nil` keeps it until dismissed — use for errors that
    /// carry an action.
    public var duration: Double?
    public var actionTitle: String?

    public init(
        id: UUID = UUID(),
        tone: FormTone = .neutral,
        title: String,
        message: String? = nil,
        duration: Double? = 4,
        actionTitle: String? = nil
    ) {
        self.id = id
        self.tone = tone
        self.title = title
        self.message = message
        self.duration = duration
        self.actionTitle = actionTitle
    }
}

/// The app's one toast queue. W9 injects it at the root and renders it with
/// `.toastOverlay(_:)`; anything with a transient message posts to it.
///
/// Toasts are for events with no home on screen. A failed run renders inline in the
/// transcript instead (spec 10 §3), and an attachment rejection renders in the tray
/// (spec 13) — do not route those here.
@MainActor
@Observable
public final class ToastCenter {
    public private(set) var toasts: [ToastMessage] = []
    /// Oldest toasts are dropped rather than stacking off-screen.
    public var maximumVisible: Int = 4

    @ObservationIgnored private var dismissals: [UUID: Task<Void, Never>] = [:]

    public init() {}

    public func post(_ toast: ToastMessage) {
        toasts.append(toast)
        if toasts.count > maximumVisible {
            let overflow = toasts.prefix(toasts.count - maximumVisible).map(\.id)
            for id in overflow { dismiss(id) }
        }
        guard let duration = toast.duration else { return }
        dismissals[toast.id] = Task { [weak self] in
            try? await Task.sleep(for: .seconds(duration))
            guard !Task.isCancelled else { return }
            self?.dismiss(toast.id)
        }
    }

    public func post(
        _ tone: FormTone,
        _ title: String,
        message: String? = nil,
        duration: Double? = 4
    ) {
        post(ToastMessage(tone: tone, title: title, message: message, duration: duration))
    }

    public func dismiss(_ id: UUID) {
        dismissals[id]?.cancel()
        dismissals[id] = nil
        toasts.removeAll { $0.id == id }
    }

    public func dismissAll() {
        for task in dismissals.values { task.cancel() }
        dismissals.removeAll()
        toasts.removeAll()
    }
}

/// One toast card.
public struct Toast: View {
    @Environment(\.theme) private var theme

    private let toast: ToastMessage
    private let onAction: (() -> Void)?
    private let onDismiss: () -> Void

    public init(_ toast: ToastMessage, onAction: (() -> Void)? = nil, onDismiss: @escaping () -> Void) {
        self.toast = toast
        self.onAction = onAction
        self.onDismiss = onDismiss
    }

    public var body: some View {
        HStack(alignment: .top, spacing: theme.metrics.spacing.md) {
            Image(systemName: toast.tone.systemImage)
                .font(.system(size: theme.metrics.iconSmall, weight: .medium))
                .foregroundStyle(toast.tone.foreground(theme.color))
                .padding(.top, 1)

            VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
                Text(toast.title)
                    .typeStyle(theme.typography.uiMedium)
                    .foregroundStyle(theme.color.textPrimary)
                if let message = toast.message {
                    Text(message)
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let actionTitle = toast.actionTitle, let onAction {
                    FormButton(actionTitle, kind: .ghost, size: .small, action: onAction)
                        .padding(.top, theme.metrics.spacing.xxs)
                        .padding(.leading, -theme.metrics.spacing.md)
                }
            }

            Spacer(minLength: 0)

            IconButton(systemImage: "xmark", accessibilityLabel: "Dismiss", size: .small, action: onDismiss)
        }
        .padding(theme.metrics.spacing.lg)
        .frame(width: theme.metrics.toastWidth, alignment: .leading)
        .background(.regularMaterial)
        .background(theme.color.surface)
        .clipShape(RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
        )
        .shadow(color: theme.color.overlay.color.opacity(0.45), radius: 14, y: 5)
        .accessibilityElement(children: .contain)
    }
}

/// The stack that renders a `ToastCenter`. Applied as an overlay at the top trailing edge
/// of the window shell (spec 09 §4).
public struct ToastStack: View {
    @Environment(\.theme) private var theme
    private let center: ToastCenter
    private let onAction: ((ToastMessage) -> Void)?

    public init(center: ToastCenter, onAction: ((ToastMessage) -> Void)? = nil) {
        self.center = center
        self.onAction = onAction
    }

    public var body: some View {
        VStack(alignment: .trailing, spacing: theme.metrics.spacing.md) {
            ForEach(center.toasts) { toast in
                Toast(toast, onAction: onAction.map { handler in { handler(toast) } }) {
                    center.dismiss(toast.id)
                }
                .transition(.move(edge: .trailing).combined(with: .opacity))
            }
        }
        .padding(theme.metrics.spacing.xl)
        .animation(theme.motion.animation(.normal, curve: .emphasized), value: center.toasts)
    }
}

public extension View {
    /// Overlays the toast stack at the top trailing edge.
    func toastOverlay(_ center: ToastCenter, onAction: ((ToastMessage) -> Void)? = nil) -> some View {
        overlay(alignment: .topTrailing) {
            ToastStack(center: center, onAction: onAction)
                .allowsHitTesting(!center.toasts.isEmpty)
        }
        .environment(center)
    }
}

#Preview("Toast") {
    ToastPreview()
}

private struct ToastPreview: View {
    @State private var center: ToastCenter = {
        let center = ToastCenter()
        center.post(ToastMessage(tone: .danger, title: "Run failed",
                                 message: "provider.unreachable — the stub harness stopped responding.",
                                 duration: nil, actionTitle: "Retry"))
        center.post(ToastMessage(tone: .success, title: "Settings exported", duration: nil))
        center.post(ToastMessage(tone: .warning, title: "Attachment skipped",
                                 message: "screenshot.heic is 12.4 MB.", duration: nil))
        return center
    }()

    var body: some View {
        ThemePreview {
            ToastStack(center: center)
        }
    }
}
