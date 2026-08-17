import FormDesign
import SwiftUI

/// `⌘/` — the shortcut cheat sheet (F12.2, spec 14 §5).
///
/// Every command in the table, grouped by category, two columns, searchable, `Esc` to
/// dismiss. It reads `ShortcutResolver`, not `AppCommands.all` directly, so a user's
/// overrides are what it shows.
public struct CheatSheet: View {
    @Environment(\.theme) private var theme
    private let center: CommandCenter

    @State private var query = ""
    @FocusState private var queryFocused: Bool

    public init(center: CommandCenter) {
        self.center = center
    }

    public var body: some View {
        ZStack {
            SheetScrim { center.dismiss(.cheatSheet) }
            SheetContainer(
                title: "Keyboard Shortcuts",
                subtitle: "Every command in form, and the keys that run it",
                onClose: { center.dismiss(.cheatSheet) }
            ) {
                VStack(spacing: 0) {
                    searchField
                    FormDivider()
                    grid(for: filteredSections)
                }
            }
        }
        .onAppear { queryFocused = true }
        .accessibilityAddTraits(.isModal)
    }

    private var searchField: some View {
        HStack(spacing: theme.metrics.spacing.md) {
            Image(systemName: "magnifyingglass")
                .typeStyle(theme.typography.caption)
                .foregroundStyle(theme.color.textTertiary)
            TextField(text: $query) {
                Text("Filter shortcuts").foregroundStyle(theme.color.textTertiary)
            }
            .textFieldStyle(.plain)
            .typeStyle(theme.typography.ui)
            .foregroundStyle(theme.color.textPrimary)
            .focused($queryFocused)
            .accessibilityLabel("Filter shortcuts")
        }
        .padding(.horizontal, theme.metrics.spacing.xl)
        .padding(.vertical, theme.metrics.spacing.md)
    }

    private struct Section: Identifiable {
        let category: CommandCategory
        let commands: [AppCommand]
        var id: String { category.rawValue }
    }

    @ViewBuilder
    private func grid(for sections: [Section]) -> some View {
        if sections.isEmpty {
            EmptyState(
                systemImage: "keyboard",
                title: "No matching shortcuts",
                message: "Nothing in the table matches “\(query)”.",
                isCompact: true
            ) { EmptyView() }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView {
                // Two columns, balanced by category rather than by row, so a category is
                // never split across the gutter.
                LazyVGrid(
                    columns: [
                        GridItem(.flexible(), spacing: theme.metrics.spacing.xl2, alignment: .top),
                        GridItem(.flexible(), spacing: theme.metrics.spacing.xl2, alignment: .top),
                    ],
                    alignment: .leading,
                    spacing: theme.metrics.spacing.xxl
                ) {
                    ForEach(sections) { section in
                        categoryColumn(section.category, section.commands)
                    }
                }
                .padding(theme.metrics.spacing.xl)
            }
        }
    }

    private func categoryColumn(
        _ category: CommandCategory, _ commands: [AppCommand]
    ) -> some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
            SectionHeader(category.title)
            ForEach(commands) { command in
                HStack(alignment: .firstTextBaseline, spacing: theme.metrics.spacing.md) {
                    Text(command.title)
                        .typeStyle(theme.typography.caption)
                        .foregroundStyle(theme.color.textPrimary)
                        .lineLimit(1)
                    Spacer(minLength: theme.metrics.spacing.md)
                    KeyCapRow(center.resolver.keys(for: command.id))
                }
                .padding(.vertical, theme.metrics.spacing.xxs)
                .accessibilityElement(children: .combine)
            }
        }
    }

    // MARK: - Filtering

    private var filteredSections: [Section] {
        CommandCategory.allCases.compactMap { category in
            let commands = AppCommands.commands(in: category).filter(matches)
            return commands.isEmpty ? nil : Section(category: category, commands: commands)
        }
    }

    private func matches(_ command: AppCommand) -> Bool {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return true }
        if FuzzyMatch.score(trimmed, in: command.title, keywords: command.keywords) != nil {
            return true
        }
        // Searching by the key itself — typing "⌘k" or "cmd+k" — is how people actually
        // look a binding up.
        return center.resolver.keys(for: command.id).contains { binding in
            binding.display.localizedCaseInsensitiveContains(trimmed)
                || binding.serialized.localizedCaseInsensitiveContains(trimmed)
        }
    }
}

#Preview("Cheat sheet") {
    CommandsPreviewHost { center in
        CheatSheet(center: center)
    } onAppear: { center in
        center.toggleCheatSheet()
    }
}
