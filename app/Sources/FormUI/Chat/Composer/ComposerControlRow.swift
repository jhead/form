import SwiftUI
import FormCore
import FormDesign

/// Under the field: a left cluster (mode, `+`, mic) and a right cluster (model, effort,
/// context ring), all 12 pt secondary — spec 08 §1.
struct ComposerControlRow: View {
    @Environment(\.theme) private var theme

    let stores: CoreStores
    let isStreaming: Bool
    let canSend: Bool
    let onSend: () -> Void
    let onStop: () -> Void
    let onAttach: () -> Void

    /// The session's override, falling back to the global default (F8.4).
    private var modelRef: ModelRef {
        stores.sessions.selected?.modelRef ?? stores.settings.settings.defaults.modelRef
    }

    var body: some View {
        HStack(spacing: theme.metrics.spacing.md) {
            // The reference's mode control. `form` has one mode, so the label and its chevron
            // are inert rather than a menu with a single item.
            HStack(spacing: theme.metrics.spacing.xs) {
                Text("Auto")
                Image(systemName: "chevron.down")
                    .typeStyle(theme.typography.micro)
            }
            .typeStyle(theme.typography.caption)
            .foregroundStyle(theme.color.textSecondary)
            .formTooltip("Agent mode", detail: "form ships a single mode")

            IconButton(
                systemImage: "plus", accessibilityLabel: "Add attachment", size: .small,
                action: onAttach)
            IconButton(
                systemImage: "mic", accessibilityLabel: "Dictate", size: .small, action: {})
                .disabled(true)

            Spacer(minLength: theme.metrics.spacing.lg)

            ModelPicker(catalog: stores.catalog, selection: modelRef) { ref in
                guard let sessionId = stores.sessions.selectedSessionId else { return }
                Task { try? await stores.sessions.setModel(sessionId, ref) }
            }

            ContextRingButton(usage: stores.chat.contextUsage)

            sendButton
        }
    }

    /// One button that becomes a stop button while a run is live (F1.6). Sending during a
    /// run is still allowed — `ChatStore` queues it (F1.7) — so the arrow stays reachable.
    @ViewBuilder
    private var sendButton: some View {
        if isStreaming {
            IconButton(
                systemImage: "stop.fill", accessibilityLabel: "Stop", size: .small,
                tone: .danger, action: onStop)
            if canSend {
                IconButton(
                    systemImage: "arrow.up.circle.fill", accessibilityLabel: "Queue message",
                    size: .small, tone: .accent, action: onSend)
            }
        } else {
            IconButton(
                systemImage: "arrow.up.circle.fill", accessibilityLabel: "Send", size: .small,
                tone: canSend ? .accent : .neutral, action: onSend)
            .disabled(!canSend)
        }
    }
}
