import SwiftUI

/// A bordered single-line field. Focus raises the border to `borderFocus` and adds the 3 pt
/// soft ring (spec 08 §1).
public struct FormTextField: View {
    @Environment(\.theme) private var theme
    @Environment(\.isEnabled) private var isEnabled

    @Binding private var text: String
    private let placeholder: String
    private let systemImage: String?
    private let isSecure: Bool
    private let size: FormControlSize
    private let onSubmit: (() -> Void)?

    @FocusState private var isFocused: Bool

    public init(
        text: Binding<String>,
        placeholder: String = "",
        systemImage: String? = nil,
        isSecure: Bool = false,
        size: FormControlSize = .medium,
        onSubmit: (() -> Void)? = nil
    ) {
        _text = text
        self.placeholder = placeholder
        self.systemImage = systemImage
        self.isSecure = isSecure
        self.size = size
        self.onSubmit = onSubmit
    }

    public var body: some View {
        HStack(spacing: theme.metrics.spacing.md) {
            if let systemImage {
                Image(systemName: systemImage)
                    .font(.system(size: theme.metrics.iconSmall))
                    .foregroundStyle(theme.color.textTertiary)
            }
            field
        }
        .padding(.horizontal, theme.metrics.spacing.lg)
        .frame(height: size.height(theme.metrics))
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .fill(theme.color.surface)
        )
        .modifier(FocusRing(isFocused: isFocused, radius: theme.metrics.radius.lg))
        .opacity(isEnabled ? 1 : 0.5)
        .animation(theme.motion.animation(.fast), value: isFocused)
    }

    @ViewBuilder
    private var field: some View {
        Group {
            if isSecure {
                SecureField(text: $text) { placeholderView }
            } else {
                TextField(text: $text) { placeholderView }
            }
        }
        .textFieldStyle(.plain)
        .typeStyle(size.typeStyle(theme.typography))
        .foregroundStyle(theme.color.textPrimary)
        .focused($isFocused)
        .onSubmit { onSubmit?() }
    }

    private var placeholderView: some View {
        Text(placeholder).foregroundStyle(theme.color.textTertiary)
    }
}

/// The shared focus treatment: a 1 pt border that lifts to `borderFocus`, plus a soft ring.
struct FocusRing: ViewModifier {
    let isFocused: Bool
    let radius: CGFloat

    @Environment(\.theme) private var theme

    func body(content: Content) -> some View {
        content
            .overlay(
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(
                        isFocused ? theme.color.borderFocus : theme.color.border,
                        lineWidth: theme.metrics.hairline * 2
                    )
            )
            .overlay(
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(
                        theme.color.borderFocus.opacity(isFocused ? 0.22 : 0),
                        lineWidth: theme.metrics.focusRing
                    )
                    .padding(-theme.metrics.focusRing / 2)
            )
    }
}

#Preview("FormTextField") {
    FormTextFieldPreview()
}

private struct FormTextFieldPreview: View {
    @State private var name = "Add a health check endpoint"
    @State private var query = ""
    @State private var key = ""

    var body: some View {
        ThemePreview {
            FormTextField(text: $name, placeholder: "Session title")
            FormTextField(text: $query, placeholder: "Search sessions…", systemImage: "magnifyingglass")
            FormTextField(text: $key, placeholder: "API key", isSecure: true)
            FormTextField(text: $query, placeholder: "Small", size: .small)
            FormTextField(text: $query, placeholder: "Disabled").disabled(true)
        }
        .frame(width: 640)
    }
}
