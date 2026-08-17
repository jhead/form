import FormDesign
import SwiftUI

/// A borderless field that replaces a label in place: `⏎` commits, `Esc` cancels, losing
/// focus commits (F2.5). Used by the sidebar's session rows and by the content header's
/// editable title (spec 09 §3, §4).
///
/// The commit-on-focus-loss rule is why this owns a draft string rather than binding straight
/// through: a cancel has to be able to throw the edit away, and a binding cannot.
struct InlineEditField: View {
    @Environment(\.theme) private var theme

    let initialText: String
    let style: TypeStyle
    let accessibilityLabel: String
    let onCommit: (String) -> Void
    let onCancel: () -> Void

    @State private var draft: String = ""
    @State private var didFinish = false
    @FocusState private var isFocused: Bool

    var body: some View {
        TextField("", text: $draft)
            .textFieldStyle(.plain)
            .typeStyle(style)
            .foregroundStyle(theme.color.textPrimary)
            .focused($isFocused)
            .accessibilityLabel(accessibilityLabel)
            .onAppear {
                draft = initialText
                isFocused = true
            }
            .onSubmit(commit)
            .onKeyPress(.escape) {
                cancel()
                return .handled
            }
            .onChange(of: isFocused) { _, focused in
                // Fires after `onSubmit`/`onKeyPress` have already finished the edit; the
                // guard is what keeps the commit from running twice.
                if !focused { commit() }
            }
    }

    private func commit() {
        guard !didFinish else { return }
        didFinish = true
        let trimmed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        // An empty title would render as a blank row, so treat it as a cancel.
        guard !trimmed.isEmpty, trimmed != initialText else {
            onCancel()
            return
        }
        onCommit(trimmed)
    }

    private func cancel() {
        guard !didFinish else { return }
        didFinish = true
        onCancel()
    }
}
