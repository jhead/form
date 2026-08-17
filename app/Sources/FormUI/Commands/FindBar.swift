import AppKit
import FormCore
import FormDesign
import SwiftUI

/// `⌘F` — the find bar, docked under the session header (F13.2, spec 14 §4).
///
/// Field, `n of m`, previous/next, case-sensitive and whole-word toggles, close. `⏎`/`⇧⏎`
/// and `⌘G`/`⌘⇧G` step; stepping past either end wraps with a small bounce. `Esc` is handled
/// by the responder chain, which closes the bar and hands focus back to the composer.
public struct FindBar: View {
    @Environment(\.theme) private var theme
    private let center: CommandCenter

    @FocusState private var queryFocused: Bool
    @State private var bounce: CGFloat = 0

    public init(center: CommandCenter) {
        self.center = center
    }

    private var find: FindController { center.find }

    public var body: some View {
        HStack(spacing: theme.metrics.spacing.md) {
            field
            count
            IconButton(
                systemImage: "chevron.up", accessibilityLabel: "Find previous", size: .small
            ) { find.previous() }
                .disabled(!find.hasMatches)
            IconButton(
                systemImage: "chevron.down", accessibilityLabel: "Find next", size: .small
            ) { find.next() }
                .disabled(!find.hasMatches)

            FormDivider(.vertical).frame(height: theme.metrics.controlHeightSmall)

            IconButton(
                systemImage: "textformat", accessibilityLabel: "Match case", size: .small,
                isActive: find.caseSensitive
            ) { find.caseSensitive.toggle() }
            IconButton(
                systemImage: "text.word.spacing", accessibilityLabel: "Whole words",
                size: .small, isActive: find.wholeWord
            ) { find.wholeWord.toggle() }

            Spacer(minLength: theme.metrics.spacing.md)

            IconButton(systemImage: "xmark", accessibilityLabel: "Close find", size: .small) {
                center.dismiss(.find)
            }
        }
        .padding(.horizontal, theme.metrics.spacing.xl)
        .frame(height: theme.metrics.headerHeight)
        .background(theme.color.surfaceRaised)
        .overlay(alignment: .bottom) { FormDivider() }
        .offset(x: bounce)
        .onAppear {
            queryFocused = true
            center.registerKeyInterceptor(id: "find", handle: handle(event:))
        }
        .onDisappear { center.unregisterKeyInterceptor(id: "find") }
        .onChange(of: find.wrapped) { _, edge in
            guard edge != nil else { return }
            playBounce(edge)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Find in session")
    }

    private var field: some View {
        HStack(spacing: theme.metrics.spacing.md) {
            Image(systemName: "magnifyingglass")
                .typeStyle(theme.typography.caption)
                .foregroundStyle(theme.color.textTertiary)
            TextField(text: Binding(get: { find.query }, set: { find.query = $0 })) {
                Text("Find in session").foregroundStyle(theme.color.textTertiary)
            }
            .textFieldStyle(.plain)
            .typeStyle(theme.typography.ui)
            .foregroundStyle(theme.color.textPrimary)
            .focused($queryFocused)
            .accessibilityLabel("Find query")
        }
        .padding(.horizontal, theme.metrics.spacing.lg)
        .frame(width: theme.metrics.composerMaxWidth / 3, height: theme.metrics.controlHeightMedium)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .fill(theme.color.surface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .strokeBorder(
                    queryFocused ? theme.color.borderFocus : theme.color.border,
                    lineWidth: theme.metrics.hairline * 2)
        )
    }

    private var count: some View {
        Text(find.positionLabel)
            .typeStyle(theme.typography.micro)
            .tabularFigures()
            .foregroundStyle(find.hasMatches ? theme.color.textSecondary : theme.color.textTertiary)
            .accessibilityLabel(
                find.hasMatches
                    ? "Match \(find.currentIndex + 1) of \(find.matches.count)"
                    : "No matches")
    }

    // MARK: - Bounce

    /// The "subtle bounce at the ends" of spec 14 §4: a single nudge in the direction the
    /// wrap came from, then back. Under reduce-motion `theme.motion.animation` returns `nil`
    /// and the offset snaps, which is the correct no-op.
    private func playBounce(_ edge: FindController.WrapEdge?) {
        let distance = theme.metrics.spacing.sm
        let target = edge == .end ? distance : -distance
        withAnimation(theme.motion.animation(.fast, curve: .emphasized)) { bounce = target }
        Task {
            try? await Task.sleep(for: .milliseconds(Int(theme.motion.fast * 1000)))
            withAnimation(theme.motion.animation(.fast, curve: .emphasized)) { bounce = 0 }
            find.clearWrap()
        }
    }

    // MARK: - Keyboard

    private func handle(event: NSEvent) -> Bool {
        guard center.isPresented(.find) else { return false }
        let modifiers = KeyModifiers(event.modifierFlags)
        guard let character = event.charactersIgnoringModifiers?.first,
              character == KeyBinding.returnKey
        else { return false }

        switch modifiers {
        case []:
            find.next()
            return true
        case .shift:
            find.previous()
            return true
        default:
            return false
        }
    }
}

#Preview("Find bar") {
    CommandsPreviewHost { center in
        VStack(spacing: 0) {
            FindBar(center: center)
            Spacer()
        }
    } onAppear: { center in
        center.openFind(seed: "ring")
    }
}
