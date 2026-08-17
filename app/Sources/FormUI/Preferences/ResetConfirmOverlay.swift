import FormDesign
import SwiftUI

/// Reset-all, behind a typed confirmation (F9.3).
///
/// A destructive action that a stray `⏎` can trigger is not a confirmation. The button stays
/// disabled until the word is typed exactly, so the gesture that destroys the document is one
/// the user could only have made on purpose.
struct ResetConfirmOverlay: View {
    @Environment(\.theme) private var theme

    static let phrase = "reset"

    let onCancel: () -> Void
    let onConfirm: () -> Void

    @State private var typed = ""

    private var matches: Bool {
        typed.trimmingCharacters(in: .whitespaces).lowercased() == Self.phrase
    }

    var body: some View {
        ZStack {
            SheetScrim(onTap: onCancel)
            SheetContainer(
                title: "Reset all settings",
                subtitle: "This cannot be undone.",
                width: theme.metrics.sheetWidth / 2,
                height: theme.metrics.sheetHeight / 2
            ) {
                VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
                    Text(
                        """
                        Every preference goes back to its default: appearance, editor, \
                        model defaults, provider settings and shortcut overrides.
                        """
                    )
                    .typeStyle(theme.typography.caption)
                    .foregroundStyle(theme.color.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)

                    Text("Your API keys stay in the Keychain and are not touched.")
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.textTertiary)

                    Text("Type \(Self.phrase) to confirm.")
                        .typeStyle(theme.typography.caption)
                        .foregroundStyle(theme.color.textPrimary)

                    FormTextField(text: $typed, placeholder: Self.phrase)
                }
                .padding(theme.metrics.spacing.xl)
            } footer: {
                FormButton("Cancel", kind: .ghost, action: onCancel)
                FormButton("Reset", kind: .destructive, action: onConfirm)
                    .disabled(!matches)
            }
        }
        .onExitCommand(perform: onCancel)
    }
}

#Preview("Reset confirm") {
    ThemePreview(padding: 0) {
        ResetConfirmOverlay(onCancel: {}, onConfirm: {})
    }
    .frame(height: 320)
}
