import AppKit
import FormCore
import FormDesign
import SwiftUI

/// `⌘K` — the command palette (F13.1, spec 14 §3).
///
/// A 640 pt panel at 20 % from the top, three capped sections, and full keyboard operation:
/// `↑`/`↓` move, `⏎` opens, `⌘⏎` opens in a new session, `Esc` dismisses. Key handling goes
/// through `CommandCenter`'s interceptor rather than `onKeyPress` so it works no matter which
/// subview holds focus, and so `⌘⏎` outranks the table's `⌘↩ Send` while the panel is up.
public struct CommandPalette: View {
    @Environment(\.theme) private var theme
    private let center: CommandCenter

    @FocusState private var queryFocused: Bool
    @State private var appeared = false

    /// The `motion.emphasized` scale-and-fade spec 14 §3 asks for. `FormDesign` has no scale
    /// token to name it with; everything else about the animation comes from `theme.motion`.
    private static let appearScale: CGFloat = 0.96

    public init(center: CommandCenter) {
        self.center = center
    }

    private var model: PaletteModel { center.palette }

    public var body: some View {
        ZStack(alignment: .top) {
            SheetScrim { center.dismiss(.palette) }
            GeometryReader { proxy in
                VStack(spacing: 0) {
                    panel.frame(width: theme.metrics.paletteWidth)
                    Spacer(minLength: 0)
                }
                .frame(maxWidth: .infinity)
                .padding(.top, proxy.size.height * theme.metrics.paletteTopFraction)
            }
        }
        .opacity(appeared ? 1 : 0)
        .scaleEffect(appeared ? 1 : Self.appearScale)
        .onAppear {
            withAnimation(theme.motion.animation(.normal, curve: .emphasized)) { appeared = true }
            queryFocused = true
            center.registerKeyInterceptor(id: "palette", handle: handle(event:))
        }
        .onDisappear { center.unregisterKeyInterceptor(id: "palette") }
        .accessibilityAddTraits(.isModal)
    }

    private var panel: some View {
        VStack(spacing: 0) {
            queryField
            FormDivider()
            results
        }
        .frame(maxHeight: theme.metrics.sheetHeight)
        .background(.regularMaterial)
        .background(theme.color.surface)
        .clipShape(RoundedRectangle(cornerRadius: theme.metrics.radius.xl, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: theme.metrics.radius.xl, style: .continuous)
                .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
        )
        .shadow(color: theme.color.overlay.color.opacity(0.5), radius: 28, y: 10)
    }

    private var queryField: some View {
        HStack(spacing: theme.metrics.spacing.lg) {
            Image(systemName: "magnifyingglass")
                .typeStyle(theme.typography.heading)
                .foregroundStyle(theme.color.textTertiary)

            TextField(text: Binding(get: { model.query }, set: { model.query = $0 })) {
                Text("Search sessions, groups and commands…")
                    .foregroundStyle(theme.color.textTertiary)
            }
            .textFieldStyle(.plain)
            .typeStyle(theme.typography.title.weighted(.regular))
            .foregroundStyle(theme.color.textPrimary)
            .focused($queryFocused)
            .accessibilityLabel("Command palette search")

            if model.isSearching {
                ProgressView().controlSize(.small)
            }
        }
        .padding(.horizontal, theme.metrics.spacing.xl)
        .padding(.vertical, theme.metrics.spacing.lg)
    }

    @ViewBuilder
    private var results: some View {
        if model.isEmpty {
            EmptyState(
                systemImage: "magnifyingglass",
                title: model.query.isEmpty ? "Start typing" : "No results",
                message: model.query.isEmpty
                    ? "Search sessions and groups, or run a command."
                    : "Nothing matched “\(model.query)”.",
                isCompact: true
            ) { EmptyView() }
                .frame(maxWidth: .infinity)
                .padding(theme.metrics.spacing.xxl)
        } else {
            ScrollViewReader { scroller in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
                        sectionView("Sessions", rows: model.sessions.map(PaletteRow.session))
                        sectionView("Commands", rows: model.commands.map(PaletteRow.command))
                        sectionView("Groups", rows: model.groups.map(PaletteRow.group))
                    }
                    .padding(theme.metrics.spacing.md)
                }
                .onChange(of: model.selection) {
                    guard let row = model.selectedRow else { return }
                    withAnimation(theme.motion.animation(.fast)) { scroller.scrollTo(row.id) }
                }
            }
        }
    }

    @ViewBuilder
    private func sectionView(_ title: String, rows: [PaletteRow]) -> some View {
        if !rows.isEmpty {
            SectionHeader(title)
                .padding(.horizontal, theme.metrics.spacing.md)
            ForEach(rows) { row in
                PaletteRowView(
                    row: row,
                    isSelected: model.selectedRow?.id == row.id,
                    binding: keyBinding(for: row),
                    onHover: { model.select(row) },
                    onActivate: { Task { await model.activate(row) } })
                    .id(row.id)
            }
        }
    }

    private func keyBinding(for row: PaletteRow) -> KeyBinding? {
        guard case let .command(item) = row else { return nil }
        return center.resolver.primaryKey(for: item.command.id)
    }

    // MARK: - Keyboard

    private func handle(event: NSEvent) -> Bool {
        guard center.topmostOverlay == .palette else { return false }
        let modifiers = KeyModifiers(event.modifierFlags)
        guard let character = event.charactersIgnoringModifiers?.first else { return false }

        switch character {
        case KeyBinding.downArrow where modifiers.isEmpty, "\t" where modifiers.isEmpty:
            model.moveSelection(by: 1)
            return true
        case KeyBinding.upArrow where modifiers.isEmpty, "\t" where modifiers == .shift:
            model.moveSelection(by: -1)
            return true
        case KeyBinding.returnKey where modifiers == .command:
            Task { await model.activateInNewSession() }
            return true
        case KeyBinding.returnKey where modifiers.isEmpty:
            Task { await model.activate() }
            return true
        default:
            return false
        }
    }
}

/// One palette line. Not a `ListRow`: a session hit is two lines tall when it carries a
/// snippet and one when it does not, and `ListRow` is a fixed-height row by design.
private struct PaletteRowView: View {
    @Environment(\.theme) private var theme

    let row: PaletteRow
    let isSelected: Bool
    let binding: KeyBinding?
    let onHover: () -> Void
    let onActivate: () -> Void

    @State private var isHovering = false

    var body: some View {
        HStack(alignment: .center, spacing: theme.metrics.spacing.lg) {
            Image(systemName: icon)
                .typeStyle(theme.typography.ui)
                .foregroundStyle(isSelected ? theme.color.accent : theme.color.textTertiary)
                .frame(width: theme.metrics.iconMedium)

            VStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
                title
                detail
            }

            Spacer(minLength: theme.metrics.spacing.md)
            trailing
        }
        .padding(.horizontal, theme.metrics.spacing.lg)
        .padding(.vertical, theme.metrics.spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .fill(fill)
        )
        .contentShape(Rectangle())
        .onHover { hovering in
            isHovering = hovering
            if hovering { onHover() }
        }
        .onTapGesture(perform: onActivate)
        .animation(theme.motion.animation(.fast), value: isSelected)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
        .accessibilityLabel(accessibilityLabel)
    }

    private var fill: ThemeColor {
        if isSelected { return theme.color.surfaceSelected }
        return theme.color.surfaceHover.opacity(isHovering ? 1 : 0)
    }

    @ViewBuilder
    private var title: some View {
        switch row {
        case let .session(item):
            Text(item.title)
                .typeStyle(theme.typography.ui)
                .foregroundStyle(theme.color.textPrimary)
                .lineLimit(1)
        case let .command(item):
            HighlightedText(
                item.command.title, ranges: item.ranges, style: \.ui, foreground: \.textPrimary)
                .lineLimit(1)
        case let .group(item):
            HighlightedText(
                item.group.name, ranges: item.ranges, style: \.ui, foreground: \.textPrimary)
                .lineLimit(1)
        }
    }

    @ViewBuilder
    private var detail: some View {
        switch row {
        case let .session(item):
            if item.snippet.isEmpty {
                Text(subtitle(for: item))
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
            } else {
                HighlightedText(item.snippet, ranges: item.ranges, style: \.micro)
                    .lineLimit(1)
            }
        case let .command(item):
            Text(item.command.category.title)
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.textTertiary)
        case let .group(item):
            Text(item.sessionCount == 1 ? "1 session" : "\(item.sessionCount) sessions")
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.textTertiary)
        }
    }

    @ViewBuilder
    private var trailing: some View {
        switch row {
        case let .session(item):
            Text(RelativeTime.string(item.timestamp))
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.textTertiary)
                .tabularFigures()
        case .command:
            if let binding { KeyCapView(binding: binding) }
        case .group:
            EmptyView()
        }
    }

    private func subtitle(for item: PaletteSessionItem) -> String {
        item.groupName ?? "Ungrouped"
    }

    private var icon: String {
        switch row {
        case .session: "bubble.left.and.text.bubble.right"
        case let .command(item): item.command.systemImage ?? "command"
        case .group: "folder"
        }
    }

    private var accessibilityLabel: String {
        switch row {
        case let .session(item): "\(item.title), \(subtitle(for: item))"
        case let .command(item):
            binding.map { "\(item.command.title), \($0.spokenDescription)" } ?? item.command.title
        case let .group(item): "\(item.group.name), \(item.sessionCount) sessions"
        }
    }
}

/// Relative timestamps for palette rows. One place, so every surface phrases "2 hr. ago" the
/// same way.
enum RelativeTime {
    static func string(_ timestamp: TimestampMs) -> String {
        let date = Date(timeIntervalSince1970: Double(timestamp) / 1000)
        return date.formatted(.relative(presentation: .numeric, unitsStyle: .abbreviated))
    }
}

#Preview("Command palette") {
    CommandsPreviewHost { center in
        CommandPalette(center: center)
    } onAppear: { center in
        center.togglePalette()
        center.palette.query = "ring"
    }
}
